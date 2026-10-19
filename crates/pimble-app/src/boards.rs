//! Boards in panes (docs/IMAGE_MARKUP_CONTRACT.md): image nodes and
//! whiteboards, made, shown, drawn on and co-edited, on the desktop and in
//! the browser from this one module.
//!
//! A board node's document is held by the pane showing it, as an editor holds
//! a note's: one `NodeDoc` per pane, loaded from the node's bytes (or from a
//! sibling pane's copy, as an editor joins from a sibling's session), with
//! `pimble_board::Board::of_node` over it. Collaboration is the editor's shape
//! exactly (CLAUDE.md "Collaboration shape"):
//!
//! * a local edit is one `Board::edit` transaction, whose update goes out as
//!   [`BackendCommand::BroadcastChanges`] (`applyEdit`, the one write path) and
//!   to every other pane of this window holding the board, a turn later;
//! * a peer's update arrives as `BackendEvent::RemoteChanges` and is applied to
//!   the document of every pane holding the node ([`apply_remote`]);
//! * the node is subscribed once however many panes hold it, and reconciled
//!   with the server's copy when it opens and when the connection comes back.
//!
//! What a pane shows is a rinch `RenderSurface` painted by
//! `pimble_board::BoardView`, the raster cache, after rinch's `paint` example:
//! every change re-renders only what it touched, and a frame is handed to the
//! surface only when something changed. The pane's editor stays mounted and
//! hidden (docs/SPLIT_VIEW_CONTRACT.md: four editors, never re-mounted).
//!
//! The parts:
//! * here: the pane's session, the collaboration plumbing, pictures, making
//!   board nodes;
//! * [`tools`]: the pointer and the keyboard (every tool, selection, moving,
//!   resizing, rotating, typing text, arranging);
//! * [`ui`]: the board view's DOM (the surface and what floats over it, the
//!   side panel with the navigator, style and layers) and the board toolbar;
//! * [`nav`]: the navigator's own surface;
//! * [`export`]: Copy Picture, Save Picture As, Save Original As, Flatten.
//!
//! Pictures on a board come from the same blob cache text pictures do
//! (`GetBlob`/`BlobLoaded`), decoded off the UI thread on the desktop.

mod export;
mod nav;
mod tools;
mod ui;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use pimble_board::{
    smooth_stroke, Align, ArrowHeads, Author, Board, BoardSnapshot, BoardTxn, BoardView, BoardWatch, CardLabel, Color, Dash,
    Geom, Kind, NewElement, Picture, PictureGeom, Placement, Point, Rect, Stamp, Style, Transform, ViewOptions,
};
use pimble_core::{node_types, BlobUrl, NodeId, StoreId};
use pimble_crdt::NodeDoc;
use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use crate::panes::{PaneId, MAX_PANES};
use crate::protocol::{BackendCommand, NodeInit, StoredBlob};
use crate::state::{ActiveEdit, AppStore};

pub(crate) use tools::delete_selected;
pub(crate) use ui::{render_board, render_board_toolbar};

// ── Tools ───────────────────────────────────────────────────────────────────

/// The board tools (contract section 4). One set for the one toolbar; they act
/// on whichever board pane has the focus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Select,
    Hand,
    Pen,
    Highlighter,
    Line,
    Arrow,
    Rect,
    Ellipse,
    Diamond,
    Text,
    Step,
    Sticky,
    Connector,
    Frame,
    Eraser,
}

impl Tool {
    /// In toolbar order.
    pub const ALL: [Tool; 15] = [
        Tool::Select,
        Tool::Hand,
        Tool::Pen,
        Tool::Highlighter,
        Tool::Line,
        Tool::Arrow,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Diamond,
        Tool::Text,
        Tool::Step,
        Tool::Sticky,
        Tool::Connector,
        Tool::Frame,
        Tool::Eraser,
    ];

    /// The tool's letter on the keyboard (contract section 4: Spectacle's
    /// where they do not collide; a board only hears plain letters while it
    /// has the keyboard, so none of them reach the app's own keys).
    pub fn key(self) -> char {
        match self {
            Tool::Select => 'v',
            Tool::Hand => 'h',
            Tool::Pen => 'p',
            Tool::Highlighter => 'm',
            Tool::Line => 'l',
            Tool::Arrow => 'a',
            Tool::Rect => 'r',
            Tool::Ellipse => 'o',
            Tool::Diamond => 'd',
            Tool::Text => 't',
            Tool::Step => 'n',
            Tool::Sticky => 's',
            Tool::Connector => 'c',
            Tool::Frame => 'f',
            Tool::Eraser => 'e',
        }
    }

    /// The tool a plain letter picks.
    pub fn from_key(key: &str) -> Option<Tool> {
        let mut chars = key.chars();
        let c = chars.next()?.to_ascii_lowercase();
        if chars.next().is_some() {
            return None;
        }
        Tool::ALL.into_iter().find(|t| t.key() == c)
    }

    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Hand => "Hand",
            Tool::Pen => "Pen",
            Tool::Highlighter => "Highlighter",
            Tool::Line => "Line",
            Tool::Arrow => "Arrow",
            Tool::Rect => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Diamond => "Diamond",
            Tool::Text => "Text",
            Tool::Step => "Numbered step",
            Tool::Sticky => "Sticky note",
            Tool::Connector => "Connector",
            Tool::Frame => "Frame",
            Tool::Eraser => "Eraser",
        }
    }

    pub fn icon(self) -> TablerIcon {
        match self {
            Tool::Select => TablerIcon::Pointer,
            Tool::Hand => TablerIcon::HandStop,
            Tool::Pen => TablerIcon::Pencil,
            Tool::Highlighter => TablerIcon::Highlight,
            Tool::Line => TablerIcon::Line,
            Tool::Arrow => TablerIcon::ArrowUpRight,
            Tool::Rect => TablerIcon::Square,
            Tool::Ellipse => TablerIcon::Circle,
            Tool::Diamond => TablerIcon::Diamond,
            Tool::Text => TablerIcon::Typography,
            Tool::Step => TablerIcon::CircleNumber1,
            Tool::Sticky => TablerIcon::Note,
            Tool::Connector => TablerIcon::Route,
            Tool::Frame => TablerIcon::Frame,
            Tool::Eraser => TablerIcon::Eraser,
        }
    }

    /// What the tool's style controls apply to: the kind it makes.
    pub fn kind(self) -> Option<Kind> {
        Some(match self {
            Tool::Pen => Kind::Pen,
            Tool::Highlighter => Kind::Highlighter,
            Tool::Line => Kind::Line,
            Tool::Arrow => Kind::Arrow,
            Tool::Rect => Kind::Rect,
            Tool::Ellipse => Kind::Ellipse,
            Tool::Diamond => Kind::Diamond,
            Tool::Text => Kind::Text,
            Tool::Step => Kind::Step,
            Tool::Sticky => Kind::Sticky,
            Tool::Connector => Kind::Connector,
            Tool::Frame => Kind::Frame,
            Tool::Select | Tool::Hand | Tool::Eraser => return None,
        })
    }
}

/// The marks' first color: the marks' red.
pub const DEFAULT_COLOR: &str = "#e53935";
/// The pen's first width, in board units.
pub const DEFAULT_WIDTH: f32 = 4.0;
/// The first font size, in board units.
pub const DEFAULT_FONT_SIZE: f32 = 24.0;
/// The quick colors the toolbar offers (the marks' red first, then the
/// appearance palette's, then black and white).
pub(crate) fn palette() -> Vec<String> {
    let mut colors = vec![DEFAULT_COLOR.to_string()];
    colors.extend(crate::appearance::COLOR_CHOICES.iter().map(|(_, c)| c.to_string()));
    colors.push("#212121".to_string());
    colors.push("#ffffff".to_string());
    colors
}
/// The widths the style panel offers (thin to extra thick), board units.
pub(crate) const WIDTHS: [f32; 4] = [2.0, 4.0, 8.0, 16.0];
/// The font sizes the style panel offers (S, M, L, XL), board units.
pub(crate) const FONT_SIZES: [(&str, f32); 4] = [("S", 16.0), ("M", 24.0), ("L", 36.0), ("XL", 56.0)];
/// The grid's spacing, in board units (contract section 4, "grid").
pub(crate) const GRID: f32 = 20.0;

/// The style new elements take, and what the style panel shows: one set for
/// the window, like the tool. Selecting elements loads the first one's style
/// here; changing a value writes it to every selected element too.
#[derive(Clone, Copy)]
pub struct StyleUi {
    pub stroke: Signal<String>,
    /// `None`: no fill.
    pub fill: Signal<Option<String>>,
    pub width: Signal<f32>,
    pub opacity: Signal<f32>,
    pub dash: Signal<Dash>,
    pub font_size: Signal<f32>,
    pub align: Signal<Align>,
    /// The arrow tool's heads (the line tool draws none).
    pub heads: Signal<ArrowHeads>,
    /// Per viewer (contract section 4): the grid shown, and snapping to it.
    pub grid: Signal<bool>,
    pub snap: Signal<bool>,
    /// The side panel (navigator, style, layers) shown beside every board.
    pub panel: Signal<bool>,
}

impl StyleUi {
    pub fn new() -> Self {
        Self {
            stroke: Signal::new(DEFAULT_COLOR.to_string()),
            fill: Signal::new(None),
            width: Signal::new(DEFAULT_WIDTH),
            opacity: Signal::new(1.0),
            dash: Signal::new(Dash::Solid),
            font_size: Signal::new(DEFAULT_FONT_SIZE),
            align: Signal::new(Align::Left),
            heads: Signal::new(ArrowHeads::End),
            grid: Signal::new(false),
            snap: Signal::new(false),
            panel: Signal::new(true),
        }
    }
}

impl Default for StyleUi {
    fn default() -> Self {
        Self::new()
    }
}

/// One layer as the layers panel shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRow {
    pub id: String,
    pub name: String,
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    pub count: usize,
}

/// The box a text being typed sits in over the board, logical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub font_px: f32,
    pub color: String,
    pub background: String,
    pub align: Align,
}

/// What a pane's board view shows over its surface, and its side panel, as
/// signals (logical pixels).
#[derive(Clone, Copy)]
pub struct PaneUi {
    /// Every selected element's outline.
    pub outlines: Signal<Vec<(f32, f32, f32, f32)>>,
    /// The selection's box, where the handles go; `None` without a
    /// selection that can be resized.
    pub handles: Signal<Option<(f32, f32, f32, f32)>>,
    /// How many elements are selected.
    pub selected: Signal<usize>,
    /// The marquee being dragged.
    pub marquee: Signal<Option<(f32, f32, f32, f32)>>,
    /// The layers, top first.
    pub layers: Signal<Vec<LayerRow>>,
    /// The layer new elements go into.
    pub layer: Signal<Option<String>>,
    /// The layer being renamed in place, and the name being typed.
    pub renaming: Signal<Option<String>>,
    pub rename_text: Signal<String>,
    /// The frames, for the navigator: (id, name).
    pub frames: Signal<Vec<(String, String)>>,
    /// The attribution under the pointer (contract section 6): where, and
    /// the sentence.
    pub tip: Signal<Option<(f32, f32, String)>>,
    /// The text being typed: its box, and what the input is given to hold
    /// (only when typing starts or a collaborator's typing merges in, never
    /// echoed back per keystroke: a write racing fast typing drops letters),
    /// with a count so the same text given twice is written twice.
    pub text_box: Signal<Option<TextBox>>,
    pub text_value: Signal<(u64, String)>,
    /// "Color by author" (contract section 6), a way of looking, per pane.
    pub by_author: Signal<bool>,
    /// The pointer is over the board (the markup control shows).
    pub hover: Signal<bool>,
    /// The navigator: the view's rectangle over it, logical pixels.
    pub nav_view: Signal<Option<(f32, f32, f32, f32)>>,
    /// Distinct authors on the board.
    pub authors: Signal<usize>,
    /// The node cards on the board: the node each links to, and the link.
    pub cards: Signal<Vec<(StoreId, NodeId, String)>>,
    /// The board has a picture selected (Save Original As...).
    pub picture_selected: Signal<bool>,
}

impl PaneUi {
    pub fn new() -> Self {
        Self {
            outlines: Signal::new(Vec::new()),
            handles: Signal::new(None),
            selected: Signal::new(0),
            marquee: Signal::new(None),
            layers: Signal::new(Vec::new()),
            layer: Signal::new(None),
            renaming: Signal::new(None),
            rename_text: Signal::new(String::new()),
            frames: Signal::new(Vec::new()),
            tip: Signal::new(None),
            text_box: Signal::new(None),
            text_value: Signal::new((0, String::new())),
            by_author: Signal::new(false),
            hover: Signal::new(false),
            nav_view: Signal::new(None),
            authors: Signal::new(0),
            cards: Signal::new(Vec::new()),
            picture_selected: Signal::new(false),
        }
    }
}

impl Default for PaneUi {
    fn default() -> Self {
        Self::new()
    }
}

/// The icon a board node made from a picture starts with (the node's ordinary
/// custom icon, so the person can change it).
pub const PHOTO_ICON: &str = "photo";
/// The icon a new whiteboard starts with.
pub const WHITEBOARD_ICON: &str = "chalkboard";

/// How long after the last wheel step the cache is re-rendered at the new zoom.
const SETTLE_MS: u32 = 150;
/// How often a stroke being drawn is appended to the document (contract
/// section 3: others watch it being drawn).
const APPEND_EVERY_MS: f64 = 100.0;
/// How often a dragged element's position is written while it moves.
const DRAG_WRITE_EVERY_MS: f64 = 80.0;
/// `PutBlob` request ids of image nodes start here, clear of the ones
/// `pictures.rs` numbers for pictures in text.
const REQUEST_BASE: u64 = 1 << 40;

/// "New Image..." on a store that refuses pictures, or with nothing picked.
const NOT_A_PICTURE: &str = "That file is not a picture Pimble can show.";
/// A tool refused on a locked layer or element (a guard against accidents,
/// contract section 5, not a permission).
const LOCKED: &str = "That is locked. Unlock it in the layers panel or with Arrange > Unlock.";

// ── Who is drawing ──────────────────────────────────────────────────────────

thread_local! {
    /// The signed-in account (contract section 6), or nobody.
    static AUTHOR: RefCell<Option<Author>> = const { RefCell::new(None) };
}

/// Who board edits are stamped as: the signed-in Pimble Cloud account (its
/// `sub` and its email's local part), or nobody when nobody is signed in.
/// The desktop calls this from `CloudStatusChanged`, the browser once its
/// session is unlocked.
pub fn set_author(user_id: Option<String>, email: Option<String>) {
    let author = match (user_id, email) {
        (Some(id), email) if !id.is_empty() => {
            let name = email.as_deref().and_then(|e| e.split('@').next()).filter(|n| !n.is_empty()).unwrap_or(id.as_str()).to_string();
            Some(Author::new(id.clone(), name))
        }
        _ => None,
    };
    tracing::info!("board edits are stamped as {:?}", author.as_ref().map(|a| &a.name));
    AUTHOR.with(|a| *a.borrow_mut() = author);
}

fn author() -> Option<Author> {
    AUTHOR.with(|a| a.borrow().clone())
}

/// Who board edits made outside a pane's board view are stamped as.
pub(crate) fn current_author() -> Option<Author> {
    author()
}

/// Milliseconds since the epoch, on both targets.
fn now_ms() -> f64 {
    #[cfg(feature = "native")]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
    }
    #[cfg(not(feature = "native"))]
    {
        js_sys::Date::now()
    }
}

/// A board timestamp: RFC 3339 UTC with milliseconds, the one shape every
/// writer uses (steps sort by it as a string).
pub(crate) fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A per-person undo manager's options (contract section 4), with a clock
/// that works in the browser too.
fn undo_options() -> yrs::undo::Options<()> {
    yrs::undo::Options {
        capture_timeout_millis: 500,
        tracked_origins: Default::default(),
        capture_transaction: None,
        timestamp: Arc::new(|| now_ms() as u64),
        init_undo_stack: Vec::new(),
        init_redo_stack: Vec::new(),
    }
}

// ── A pane's board ──────────────────────────────────────────────────────────

/// An element as a gesture found it: what the gesture writes is computed from
/// this, never from the moving document.
#[derive(Debug, Clone)]
struct Base {
    id: String,
    kind: Kind,
    geom: Geom,
    transform: Transform,
    /// Its own geometry's extent (the pivot of its rotation and scale),
    /// element space.
    pivot: Rect,
    /// Its parent's world transform as a translation (frames and groups are
    /// moved, not rotated, by the tools).
    parent_offset: (f32, f32),
    /// Its world transform is a plain translation (no rotation or scale
    /// anywhere above it): its geometry can be edited directly.
    plain: bool,
    /// Its bounds in board units.
    bounds: Rect,
}

/// A resize or rotate handle of the selection box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
    Rotate,
}

/// What the pointer is doing on a board.
enum Gesture {
    Idle,
    /// Middle-drag, space-drag or the hand tool: the last position, physical
    /// pixels.
    Pan { last: (f32, f32) },
    /// A pen or highlighter stroke: its element once it is written (its
    /// first ~100 ms of points make it, so a stroke cancelled at once, a
    /// finger that turns out to be the first of a pinch, writes nothing),
    /// what it is made with, and the points not yet appended.
    Stroke { id: Option<String>, kind: Kind, style: Style, layer: String, pending: Vec<[f32; 3]>, appended_at: f64 },
    /// A line, arrow, shape or frame being dragged out from `start`: its
    /// element once the drag has gone far enough to make one.
    Shape { kind: Kind, start: Point, last: Point, id: Option<String>, written_at: f64 },
    /// A connector being dragged from `from`.
    Connector { from: pimble_board::End, last: Point, id: Option<String>, written_at: f64 },
    /// Moving the selection.
    Move { start: Point, last: Point, bases: Vec<Base>, written_at: f64, clicked: Option<String> },
    /// Resizing the selection by a handle.
    Resize { handle: Handle, bounds: Rect, last: Point, bases: Vec<Base>, written_at: f64 },
    /// Rotating the selection about `center`.
    Rotate { center: Point, start: f32, last: f32, bases: Vec<Base>, written_at: f64 },
    /// A marquee from `start` (physical pixels); `base` is the selection it
    /// adds to (shift).
    Marquee { start: (f32, f32), now: (f32, f32), base: Vec<String> },
    /// The eraser, held down.
    Erase,
    /// Two fingers down: their moves pan, their spread zooms (`Pinch`),
    /// until the last of them is lifted.
    Pinch,
}

/// A text being typed into an element (contract section 4: "typed in a real
/// input overlaid on the surface"). `id` is `None` until the first character
/// makes the element.
struct TextEdit {
    id: Option<String>,
    kind: Kind,
    at: Point,
    /// What the input held after the last keystroke.
    last: String,
}

/// One pane's board: the node's document, the board in it, and the view.
struct Session {
    store_id: StoreId,
    node_id: NodeId,
    doc: NodeDoc,
    board: Board,
    /// `None` until the board exists here (its creating update may still be
    /// on its way).
    watch: Option<BoardWatch>,
    undo: Option<yrs::undo::UndoManager<()>>,
    view: BoardView,
    /// Fitted to the visible extent once, the first time it showed a board.
    fitted: bool,
    gesture: Gesture,
    /// The selection, in the order it was made.
    selected: Vec<String>,
    /// The space bar is held: a left drag pans.
    space: bool,
    /// Shift is held: constrain, add to the selection.
    shift: bool,
    /// Bumped by every wheel step; the settle timer of an older one does nothing.
    settle_generation: u64,
    /// The last press: when, where (physical pixels), for double-clicks.
    last_press: (f64, f32, f32),
    /// The pointer's last position over the surface, physical pixels.
    pointer: (f32, f32),
    /// The pointer and button whose press the gesture is (one at a time:
    /// another pointer's or button's presses meanwhile start nothing).
    driver: Option<(u64, SurfaceMouseButton)>,
    /// The fingers on the surface, in the order they came down, with where
    /// they are (physical pixels).
    touches: Vec<(u64, (f32, f32))>,
    /// The pressure of the pointer drawing (0.5 for a mouse).
    pressure: f32,
    text: Option<TextEdit>,
    /// The way of looking (color by author, card titles).
    options: ViewOptions,
    /// Bumped by every change; the navigator's timer of an older one does
    /// nothing.
    nav_generation: u64,
    /// The tool this board opens on has been picked (once its board is here).
    tool_chosen: bool,
    /// The board view is over a note's text, marking up one of its pictures
    /// (contract section 2): the board is `data["board:<id>"]` of the note.
    markup: Option<Markup>,
}

/// A picture in text being marked up in a pane.
struct Markup {
    /// The picture's `src`, by which its atom is found again.
    src: String,
    /// Where the atom was when the view opened (the nearest such picture to
    /// it is the one).
    pos: usize,
    board_id: String,
    /// The board, made in this pane's copy of the note when the view opened
    /// so the picture shows, and sent with the first mark ("the first mark
    /// mints the board id"): opening and closing the view writes nothing.
    pending: Option<Vec<u8>>,
    /// The atom's `board` is still to be written (with the first mark).
    attr_needed: bool,
}

impl Session {
    fn snapshot(&self) -> &BoardSnapshot {
        self.view.snapshot()
    }

    /// Bring the view up to date with the document after any change to it:
    /// only what changed is re-read, once the board exists.
    fn sync_view(&mut self) {
        match &self.watch {
            Some(watch) => {
                let changes = watch.take();
                self.view.update(&self.board, &changes);
            }
            None => {
                if let Some(watch) = self.board.watch() {
                    self.watch = Some(watch);
                    let snapshot = self.board.snapshot().with_options(self.options.clone(), &self.board);
                    self.view.set_snapshot(snapshot);
                }
            }
        }
        if self.undo.is_none() {
            self.undo = self.board.undo_manager(undo_options());
        }
        let snapshot = self.view.snapshot();
        self.selected.retain(|id| snapshot.get(id).is_some());
    }

    /// A board rectangle on screen, physical pixels (left, top, width, height).
    fn to_screen(&self, r: &Rect) -> (f32, f32, f32, f32) {
        let (x0, y0) = self.view.board_to_screen(Point::new(r.x, r.y));
        let (x1, y1) = self.view.board_to_screen(Point::new(r.right(), r.bottom()));
        (x0, y0, x1 - x0, y1 - y0)
    }

    /// The union of the selected elements' bounds.
    fn selection_bounds(&self) -> Option<Rect> {
        let snapshot = self.snapshot();
        self.selected.iter().filter_map(|id| snapshot.get(id)).map(|e| e.bounds).reduce(|a, b| a.union(&b))
    }

    /// Whether element `id` may be changed by the tools: neither it nor its
    /// layer is locked (contract section 5).
    fn editable(&self, id: &str) -> bool {
        let snapshot = self.snapshot();
        let Some(e) = snapshot.get(id) else { return false };
        !e.raw.locked && !snapshot.layer(&e.layer).is_some_and(|l| l.locked)
    }
}

thread_local! {
    static SESSIONS: RefCell<[Option<Session>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// Each pane slot's surface, made once (the slot is mounted once).
    static SURFACES: RefCell<[Option<RenderSurfaceHandle>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// Each surface's box in logical pixels, for the desktop's pixel scale
    /// (pointer events are logical, frames physical).
    static BOUNDS: RefCell<[Option<Signal<rinch::core::ElementBounds>>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// The app, for the callbacks rinch calls with nothing in hand.
    static APP: Cell<Option<AppStore>> = const { Cell::new(None) };
    /// The tree's drag (a row's tree value), so a row dropped on a board
    /// becomes a node card (contract section 4).
    static TREE_DRAG: Cell<Option<DragContext<String>>> = const { Cell::new(None) };
}

/// The tree's drag-and-drop context, handed over once by the app.
pub(crate) fn set_tree_drag(ctx: DragContext<String>) {
    TREE_DRAG.with(|d| d.set(Some(ctx)));
}

/// The tree row being dragged (its tree value), when a drop takes it.
/// `take` decides whether it is taken: a drop that does not want it leaves
/// it to whoever else might.
pub(crate) fn take_tree_drag(take: impl FnOnce(&str) -> bool) -> bool {
    let Some(ctx) = TREE_DRAG.with(Cell::get) else { return false };
    let Some(value) = ctx.get() else { return false };
    if !take(&value) {
        return false;
    }
    ctx.take();
    true
}

fn with_session<R>(pane: PaneId, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()].as_mut().map(f))
}

fn surface_of(pane: PaneId) -> Option<RenderSurfaceHandle> {
    SURFACES.with(|s| s.borrow()[pane.index()].clone())
}

/// Logical to physical pixels on pane `pane`'s surface.
fn scale_of(pane: PaneId) -> f32 {
    #[cfg(not(feature = "native"))]
    {
        let _ = pane;
        web_sys::window().map(|w| w.device_pixel_ratio() as f32).filter(|s| *s > 0.0).unwrap_or(1.0)
    }
    #[cfg(feature = "native")]
    {
        let physical = surface_of(pane).map(|s| s.layout_size().0).unwrap_or(0);
        let logical = BOUNDS.with(|b| b.borrow()[pane.index()]).map(|sig| untracked(|| sig.get().width)).unwrap_or(0.0);
        if physical > 0 && logical > 0.0 {
            physical as f32 / logical
        } else {
            1.0
        }
    }
}

/// The board's background on the current theme.
fn background(dark: bool) -> Color {
    if dark {
        Color::rgb(0x1f, 0x20, 0x24)
    } else {
        Color::rgb(0xf8, 0xf9, 0xfa)
    }
}

/// Start showing board node `node_id` in pane `pane`: hold its document,
/// subscribe to it (once per node), and reconcile with the server's copy.
/// `content` is the node's document as the cache has it; another pane of
/// this window holding the node is the fresher copy and is joined instead.
pub(crate) fn start(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, content: &[u8]) {
    // Whatever the pane held goes first: its editor's session, or a board.
    crate::editor::stop_editing(store, pane);
    APP.with(|app| app.set(Some(store)));
    let first = !store.is_editing(store_id, node_id);
    let sibling = store
        .panes_boarding(store_id, node_id)
        .into_iter()
        .find_map(|other| with_session(other, |s| s.doc.save()));
    let bytes = sibling.as_deref().unwrap_or(content);
    let doc = match NodeDoc::load(bytes) {
        Ok(doc) => doc,
        Err(e) => {
            tracing::warn!("board {node_id} could not be read ({e}); showing it empty until the server's copy arrives");
            NodeDoc::new()
        }
    };
    let board = Board::of_node(&doc).with_origin(format!("pimble-board-pane-{}", pane.index()));
    let dark = untracked(|| store.dark_mode.get());
    let ui = store.pane(pane).board_ui;
    let mut view = BoardView::new(1, 1, background(dark));
    if untracked(|| store.board_style.grid.get()) {
        view.set_grid(Some(GRID));
    }
    let mut session = Session {
        store_id,
        node_id,
        doc,
        board,
        watch: None,
        undo: None,
        view,
        fitted: false,
        gesture: Gesture::Idle,
        selected: Vec::new(),
        space: false,
        shift: false,
        settle_generation: 0,
        last_press: (0.0, 0.0, 0.0),
        pointer: (0.0, 0.0),
        driver: None,
        touches: Vec::new(),
        pressure: 0.5,
        text: None,
        options: ViewOptions { color_by_author: untracked(|| ui.by_author.get()), ..ViewOptions::default() },
        nav_generation: 0,
        tool_chosen: false,
        markup: None,
    };
    session.sync_view();
    SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()] = Some(session));
    let state = store.pane(pane);
    state.board.set(Some(ActiveEdit { store_id, node_id }));
    ui.layer.set(None);
    if first {
        store.send(BackendCommand::SubscribeNodeChanges { store_id, node_id });
    }
    request_reconcile(store, pane);
    after_change(store, pane);
    crate::pictures::retry_missing();
}

/// Stop showing pane `pane`'s board, if it shows one: the node cache gets the
/// document as this pane last had it, and the last pane holding the node lets
/// go of its subscription.
pub(crate) fn stop(store: AppStore, pane: PaneId) {
    tools::end_text(store, pane);
    let Some(session) = SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()].take()) else { return };
    let state = store.pane(pane);
    if session.markup.is_some() {
        // The note's own editor holds the note: the board view over it goes,
        // and the text shows again. The pane's copy (which may hold a board
        // made for a mark that never came) is dropped.
        state.markup.set(false);
        state.show_board.set(false);
    } else if let Some(sig) = store.get_node_signal(session.store_id, session.node_id) {
        let bytes = session.doc.save();
        sig.update(|node| node.content = bytes);
    }
    state.board.set(None);
    state.board_marks.set((0, true));
    let ui = state.board_ui;
    ui.outlines.set(Vec::new());
    ui.handles.set(None);
    ui.selected.set(0);
    ui.marquee.set(None);
    ui.tip.set(None);
    ui.layers.set(Vec::new());
    ui.frames.set(Vec::new());
    if !store.is_editing(session.store_id, session.node_id) {
        store.send(BackendCommand::UnsubscribeNodeChanges { store_id: session.store_id, node_id: session.node_id });
    }
}

/// Ask the server for what it has beyond pane `pane`'s copy of its board.
pub(crate) fn request_reconcile(store: AppStore, pane: PaneId) {
    if let Some((store_id, node_id, state_vector)) = with_session(pane, |s| (s.store_id, s.node_id, s.doc.state_vector())) {
        store.send(BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector });
    }
}

/// Every board pane holding the node asks again (the connection came back).
pub(crate) fn request_reconcile_all(store: AppStore, store_id: StoreId, node_id: NodeId) {
    for pane in store.panes_boarding(store_id, node_id) {
        request_reconcile(store, pane);
    }
}

/// The server's answer to a reconcile: merge it into every board pane
/// holding the node, then send back what the server lacks.
pub(crate) fn apply_reconcile(store: AppStore, store_id: StoreId, node_id: NodeId, diff: &[u8], server_state_vector: &[u8]) {
    let writable = store.node_access(store_id, node_id).allows_write();
    for pane in store.panes_boarding(store_id, node_id) {
        let ours = with_session(pane, |s| {
            if !diff.is_empty() {
                if let Err(e) = s.doc.apply_update(diff) {
                    tracing::warn!("a board's reconcile did not apply: {e}");
                }
                s.sync_view();
            }
            if writable {
                s.doc.diff_if_peer_lacks_it(server_state_vector, diff).ok().flatten()
            } else {
                None
            }
        })
        .flatten();
        if let Some(ours) = ours {
            use base64::Engine;
            let changes = base64::engine::general_purpose::STANDARD.encode(&ours);
            store.send(BackendCommand::BroadcastChanges { store_id, node_id, changes });
        }
        after_change(store, pane);
    }
}

/// A peer's update of a node: applied to the board of every pane holding it.
pub(crate) fn apply_remote(store: AppStore, store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    for pane in store.panes_boarding(store_id, node_id) {
        apply_to(store, pane, store_id, node_id, bytes);
    }
}

fn apply_to(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    let applied = with_session(pane, |s| {
        if (s.store_id, s.node_id) != (store_id, node_id) {
            return false;
        }
        if let Err(e) = s.doc.apply_update(bytes) {
            tracing::warn!("a board update did not apply: {e}");
        }
        s.sync_view();
        true
    });
    if applied == Some(true) {
        tools::text_changed_elsewhere(store, pane);
        after_change(store, pane);
    }
}

/// Send an update this pane made: to the server through `applyEdit`, and to
/// every other pane of this window holding the board, a turn later (the
/// server relays to other clients, not back to this one).
fn send_update(store: AppStore, from: PaneId, store_id: StoreId, node_id: NodeId, bytes: Vec<u8>) {
    use base64::Engine;
    let changes = base64::engine::general_purpose::STANDARD.encode(&bytes);
    store.send(BackendCommand::BroadcastChanges { store_id, node_id, changes });
    // A board shown in text (a picture's markup, an embed) follows.
    crate::text_boards::local_update(store_id, node_id, &bytes);
    let others: Vec<PaneId> = store.panes_boarding(store_id, node_id).into_iter().filter(|p| *p != from).collect();
    if others.is_empty() {
        return;
    }
    let deliver = move || {
        for other in others {
            apply_to(store, other, store_id, node_id, &bytes);
        }
    };
    #[cfg(test)]
    deliver();
    #[cfg(not(test))]
    let _ = set_timeout(0, deliver);
}

/// Send an update of node `node_id` made outside any pane's board view (the
/// markup toggle over a picture in text or an embed, a board copied in with a
/// paste): to the server, to every board pane holding the node, and to what
/// shows it in text.
pub(crate) fn send_node_update(store: AppStore, store_id: StoreId, node_id: NodeId, bytes: Vec<u8>) {
    use base64::Engine;
    let changes = base64::engine::general_purpose::STANDARD.encode(&bytes);
    store.send(BackendCommand::BroadcastChanges { store_id, node_id, changes });
    crate::text_boards::local_update(store_id, node_id, &bytes);
    for pane in store.panes_boarding(store_id, node_id) {
        apply_to(store, pane, store_id, node_id, &bytes);
    }
}

/// The first mark on a picture in text: the board made when the view
/// opened goes out first, then the atom's `board` is written, so the text
/// names a board that exists.
fn first_mark(store: AppStore, pane: PaneId, held: (StoreId, NodeId)) {
    let Some((pending, attr)) = with_session(pane, |s| {
        let markup = s.markup.as_mut()?;
        let pending = markup.pending.take();
        let attr = std::mem::take(&mut markup.attr_needed).then(|| (markup.src.clone(), markup.pos, markup.board_id.clone()));
        Some((pending, attr))
    })
    .flatten() else {
        return;
    };
    if let Some(bytes) = pending {
        send_update(store, pane, held.0, held.1, bytes);
    }
    if let Some((src, pos, board)) = attr {
        if !crate::text_boards::write_board_attr(pane, &src, pos, &board) {
            tracing::warn!("the picture {src} was not in the text any more; its board {board} stays in the note's data");
        }
    }
}

/// "Mark Up..." on a picture in pane `pane`'s text (contract section 2):
/// the board view over the pane, on the picture's board in the note's
/// `data["board:<id>"]`. A picture with no board yet gets one in this pane's copy
/// of the note, shown at once and written with the first mark.
pub(crate) fn start_markup(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, src: &str, pos: usize, board: Option<String>) {
    let Some(blob) = BlobUrl::parse(src) else { return };
    if !store.node_access(store_id, node_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    stop(store, pane);
    APP.with(|app| app.set(Some(store)));
    let bytes = crate::text_boards::host_bytes(store_id, node_id)
        .or_else(|| crate::editor::editor(pane).collab_snapshot())
        .unwrap_or_default();
    let doc = NodeDoc::load(&bytes).unwrap_or_else(|_| NodeDoc::new());
    let attr_needed = board.is_none();
    let board_id = board.unwrap_or_else(pimble_board::doc::new_id);
    let board = Board::in_text(&doc, &board_id).with_origin(format!("pimble-board-pane-{}", pane.index()));
    let pending = if board.exists() {
        None
    } else {
        // The picture at one unit per pixel in a locked layer of its own,
        // and a layer above it to draw in, as an image node starts.
        let size = crate::pictures::held_bytes(&blob)
            .and_then(|bytes| Picture::decode(&bytes).ok())
            .map(|p| (p.width(), p.height()))
            .unwrap_or((640, 480));
        let author = author();
        let now = now_stamp();
        let made = Board::in_text(&doc, &board_id).edit(Stamp::new(author.as_ref(), &now), |t| {
            t.create()?;
            let layer = t.add_layer("Picture", Placement::Bottom)?;
            let geom = PictureGeom { rect: Rect::new(0.0, 0.0, size.0 as f32, size.1 as f32), src: blob.to_string(), px_w: size.0, px_h: size.1, crop: None };
            t.add_element(NewElement::new(Kind::Picture, Geom::Picture(geom)).in_layer(layer.clone()))?;
            t.set_layer_locked(&layer, true)?;
            Ok(())
        });
        made.update
    };
    let dark = untracked(|| store.dark_mode.get());
    let ui = store.pane(pane).board_ui;
    let mut view = BoardView::new(1, 1, background(dark));
    if untracked(|| store.board_style.grid.get()) {
        view.set_grid(Some(GRID));
    }
    let mut session = Session {
        store_id,
        node_id,
        doc,
        board,
        watch: None,
        undo: None,
        view,
        fitted: false,
        gesture: Gesture::Idle,
        selected: Vec::new(),
        space: false,
        shift: false,
        settle_generation: 0,
        last_press: (0.0, 0.0, 0.0),
        pointer: (0.0, 0.0),
        driver: None,
        touches: Vec::new(),
        pressure: 0.5,
        text: None,
        options: ViewOptions { color_by_author: untracked(|| ui.by_author.get()), ..ViewOptions::default() },
        nav_generation: 0,
        tool_chosen: false,
        markup: Some(Markup { src: src.to_string(), pos, board_id, pending, attr_needed }),
    };
    // The watch and the undo manager start after the board was made here,
    // so undo never takes back the board itself.
    session.sync_view();
    SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()] = Some(session));
    let state = store.pane(pane);
    state.board.set(Some(ActiveEdit { store_id, node_id }));
    state.markup.set(true);
    state.show_board.set(true);
    ui.layer.set(None);
    after_change(store, pane);
    crate::pictures::retry_missing();
    // The keyboard goes to the board (its tools' letters, Escape).
    crate::editor::editor(pane).blur();
}

/// A mark on pane `pane`'s board, as a tool would make one (tests).
#[cfg(test)]
pub(crate) fn mark_for_test(store: AppStore, pane: PaneId) {
    local_edit(store, pane, |t| t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(10.0, 10.0, 20.0, 20.0)))));
}

/// "Done" on the board view over pane `pane`'s text: back to the text.
pub(crate) fn end_markup(store: AppStore, pane: PaneId) {
    if with_session(pane, |s| s.markup.is_some()) != Some(true) {
        return;
    }
    stop(store, pane);
    crate::toolbar::bump_toolbar(pane);
    crate::editor::editor(pane).focus();
}

/// The decoded pictures `snapshot` names, for drawing it elsewhere (a
/// picture in text, an embed), and whether every one is here; the missing
/// ones are fetched, and `text_boards::picture_arrived` hears when they are.
pub(crate) fn pictures_of(snapshot: &BoardSnapshot) -> (HashMap<String, Arc<Picture>>, bool) {
    let source = picture_source();
    let mut found = HashMap::new();
    let mut all = true;
    for e in &snapshot.elements {
        if let Geom::Picture(g) = &e.raw.geom {
            match source(&g.src) {
                Some(picture) => {
                    found.insert(g.src.clone(), picture);
                }
                None => all = false,
            }
        }
    }
    if !all {
        request_wanted_pictures();
    }
    (found, all)
}

/// Whether this device may write pane `pane`'s board; says the read-only
/// sentence when it may not.
fn writable_or_say(store: AppStore, pane: PaneId) -> bool {
    let Some(held) = with_session(pane, |s| (s.store_id, s.node_id)) else { return false };
    if store.node_access(held.0, held.1).allows_write() {
        return true;
    }
    crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
    false
}

/// One local edit of pane `pane`'s board, stamped with who and when: refused
/// (with the sentence) when this device may only read the node. Answers the
/// closure's result.
fn local_edit<R>(store: AppStore, pane: PaneId, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> pimble_board::doc::Result<R>) -> Option<R> {
    let held = with_session(pane, |s| (s.store_id, s.node_id))?;
    if !writable_or_say(store, pane) {
        return None;
    }
    first_mark(store, pane, held);
    let author = author();
    let now = now_stamp();
    let (result, update) = with_session(pane, |s| {
        let edited = s.board.edit(Stamp::new(author.as_ref(), &now), f);
        s.sync_view();
        (edited.result, edited.update)
    })?;
    if let Some(bytes) = update {
        send_update(store, pane, held.0, held.1, bytes);
    }
    after_change(store, pane);
    match result {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::info!("a board edit was refused: {e}");
            None
        }
    }
}

/// Start a new undo step: the next edit is taken back on its own, however
/// soon after the last.
fn new_undo_step(pane: PaneId) {
    with_session(pane, |s| {
        if let Some(undo) = s.undo.as_mut() {
            undo.reset();
        }
    });
}

/// After the pane's board or view changed: what floats over the board and the
/// side panel follow, then the surface is handed a frame if one is due.
fn after_change(store: AppStore, pane: PaneId) {
    let scale = scale_of(pane);
    let state = store.pane(pane);
    let ui = state.board_ui;
    let published = with_session(pane, |s| {
        let snapshot = s.snapshot();
        let marks = (snapshot.markup_count(), snapshot.markup_visible);
        let logical = |r: (f32, f32, f32, f32)| (r.0 / scale, r.1 / scale, r.2 / scale, r.3 / scale);
        let outlines: Vec<(f32, f32, f32, f32)> =
            s.selected.iter().filter_map(|id| snapshot.get(id)).map(|e| logical(s.to_screen(&e.bounds))).collect();
        let handles = if s.selected.iter().any(|id| s.editable(id)) {
            s.selection_bounds().map(|b| logical(s.to_screen(&b)))
        } else {
            None
        };
        let layers: Vec<LayerRow> = snapshot
            .layers
            .iter()
            .rev()
            .map(|l| LayerRow { id: l.id.clone(), name: l.name.clone(), visible: l.visible, locked: l.locked, opacity: l.opacity, count: l.count })
            .collect();
        let frames: Vec<(String, String)> = snapshot
            .frames()
            .into_iter()
            .map(|e| (e.raw.id.clone(), e.raw.text.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "Frame".to_string())))
            .collect();
        let picture = s.selected.len() == 1 && snapshot.get(&s.selected[0]).is_some_and(|e| e.raw.kind.is_picture());
        let authors = snapshot.authors().len();
        let marquee = match &s.gesture {
            Gesture::Marquee { start, now, .. } => {
                let (x0, y0) = (start.0.min(now.0), start.1.min(now.1));
                Some(logical((x0, y0, (start.0 - now.0).abs(), (start.1 - now.1).abs())))
            }
            _ => None,
        };
        (marks, outlines, handles, layers, frames, picture, authors, marquee, s.selected.len())
    });
    // An image opens on the arrow, a whiteboard on select (contract section
    // 4), once its board is here.
    let opening = with_session(pane, |s| {
        if s.tool_chosen || !s.snapshot().exists {
            return None;
        }
        s.tool_chosen = true;
        Some(s.snapshot().elements.iter().any(|e| e.raw.kind.is_picture()))
    })
    .flatten();
    if let Some(picture) = opening {
        if untracked(|| store.focused_pane.get()) == pane {
            store.board_tool.set(if picture { Tool::Arrow } else { Tool::Select });
        }
    }
    match published {
        Some((marks, outlines, handles, layers, frames, picture, authors, marquee, count)) => {
            state.board_marks.set_if_changed(marks);
            ui.outlines.set_if_changed(outlines);
            ui.handles.set_if_changed(handles);
            ui.selected.set_if_changed(count);
            // The selected layer must still exist.
            let chosen = untracked(|| ui.layer.get());
            if chosen.as_ref().is_some_and(|id| !layers.iter().any(|l| &l.id == id)) {
                ui.layer.set(None);
            }
            ui.layers.set_if_changed(layers);
            ui.frames.set_if_changed(frames);
            ui.picture_selected.set_if_changed(picture);
            ui.authors.set_if_changed(authors);
            ui.marquee.set_if_changed(marquee);
            tools::place_text_box(store, pane);
            ui::refresh_card_labels(store, pane);
        }
        None => {
            ui.outlines.set_if_changed(Vec::new());
            ui.handles.set_if_changed(None);
        }
    }
    present(pane);
    nav::changed(store, pane);
}

/// Hand pane `pane`'s surface its next frame, if anything changed: the
/// view at the surface's own physical size, fitted to the board the first
/// time there is one to fit.
fn present(pane: PaneId) {
    let Some(surface) = surface_of(pane) else { return };
    let (width, height) = surface.layout_size();
    if width == 0 || height == 0 {
        return;
    }
    let scale = scale_of(pane);
    let writer = surface.writer();
    with_session(pane, |s| {
        s.view.resize(width, height);
        if !s.fitted && s.view.snapshot().exists {
            match s.view.snapshot().visible_extent() {
                Some(extent) => {
                    s.view.fit(&extent, 24.0 * scale);
                    // Never larger than one board unit to a CSS pixel: a
                    // small picture opens at its own size.
                    if s.view.zoom() > scale {
                        s.view.set_camera(extent.center(), scale);
                    }
                }
                None => s.view.set_camera(Point::new(0.0, 0.0), scale),
            }
            s.fitted = true;
            // The first frame is at the fitted zoom, not a resample.
            s.view.settle(&picture_source());
        }
        if let Some(frame) = s.view.frame(&picture_source()) {
            writer.submit_frame(frame, width, height);
        }
    });
    request_wanted_pictures();
}

/// The camera moved: the floating boxes follow, a frame is due.
fn camera_moved(store: AppStore, pane: PaneId) {
    after_change(store, pane);
}

// ── Pictures on boards ──────────────────────────────────────────────────────

#[derive(Default)]
struct Pictures {
    /// Decoded, by `src`.
    decoded: HashMap<String, Arc<Picture>>,
    /// Their bytes as stored, by `src` (Save Original As..., SVG export).
    raw: HashMap<String, Arc<Vec<u8>>>,
    /// Asked for by a render and not yet fetched.
    wanted: HashSet<String>,
    /// Asked of the backend (or being decoded).
    asked: HashSet<String>,
}

thread_local! {
    static PICTURES: RefCell<Pictures> = RefCell::new(Pictures::default());
}

/// The renderer's pictures: the decoded ones, and a note of every one it
/// wanted and did not find.
fn picture_source() -> impl Fn(&str) -> Option<Arc<Picture>> {
    |src: &str| {
        PICTURES.with(|p| {
            let mut p = p.borrow_mut();
            match p.decoded.get(src) {
                Some(picture) => Some(picture.clone()),
                None => {
                    if !p.asked.contains(src) {
                        p.wanted.insert(src.to_string());
                    }
                    None
                }
            }
        })
    }
}

/// A picture's bytes as stored, when this window has them.
fn picture_bytes(src: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = PICTURES.with(|p| p.borrow().raw.get(src).cloned()) {
        return Some(bytes.as_ref().clone());
    }
    let url = BlobUrl::parse(src)?;
    crate::pictures::held_bytes(&url).map(|b| b.as_ref().clone())
}

/// Fetch what the last frames wanted: from the text pictures' cache when it
/// has the bytes, else from the backend.
fn request_wanted_pictures() {
    let wanted: Vec<String> = PICTURES.with(|p| {
        let mut p = p.borrow_mut();
        let wanted: Vec<String> = p.wanted.drain().collect();
        p.asked.extend(wanted.iter().cloned());
        wanted
    });
    for src in wanted {
        let Some(url) = BlobUrl::parse(&src) else { continue };
        match crate::pictures::held_bytes(&url) {
            Some(bytes) => decode(src, bytes),
            None => {
                if let Some(store) = APP.with(Cell::get) {
                    store.send(BackendCommand::GetBlob { url });
                }
            }
        }
    }
}

/// The answer to a `GetBlob`: a board waiting for that picture gets it.
pub(crate) fn blob_loaded(url: &BlobUrl, result: &Result<Arc<Vec<u8>>, String>) {
    let src = url.to_string();
    if !PICTURES.with(|p| p.borrow().asked.contains(&src)) {
        return;
    }
    match result {
        Ok(bytes) => decode(src, bytes.clone()),
        // Not here (yet): asked again the next time a board opens.
        Err(_) => PICTURES.with(|p| {
            p.borrow_mut().asked.remove(&src);
        }),
    }
}

/// Decode a picture's bytes, off the UI thread on the desktop, and show it on
/// every board that names it.
fn decode(src: String, bytes: Arc<Vec<u8>>) {
    PICTURES.with(|p| {
        p.borrow_mut().raw.insert(src.clone(), bytes.clone());
    });
    #[cfg(feature = "native")]
    std::thread::spawn(move || {
        let picture = Picture::decode(&bytes);
        run_on_main_thread(move || installed(src, picture.map(Arc::new).map_err(|e| e.to_string())));
    });
    #[cfg(not(feature = "native"))]
    {
        let _ = set_timeout(0, move || installed(src, Picture::decode(&bytes).map(Arc::new).map_err(|e| e.to_string())));
    }
}

fn installed(src: String, picture: Result<Arc<Picture>, String>) {
    match picture {
        Ok(picture) => {
            PICTURES.with(|p| {
                p.borrow_mut().decoded.insert(src.clone(), picture);
            });
            crate::text_boards::picture_arrived();
        }
        Err(e) => {
            tracing::warn!("the picture {src} could not be decoded: {e}");
            return;
        }
    }
    for pane in PaneId::ALL {
        let touched = with_session(pane, |s| {
            let rects: Vec<Rect> = s
                .view
                .snapshot()
                .elements
                .iter()
                .filter(|e| matches!(&e.raw.geom, Geom::Picture(g) if g.src == src))
                .map(|e| e.bounds)
                .collect();
            for rect in &rects {
                s.view.invalidate(*rect);
            }
            s.nav_generation += 1;
            !rects.is_empty()
        });
        if touched == Some(true) {
            present(pane);
            if let Some(store) = APP.with(Cell::get) {
                nav::changed(store, pane);
            }
        }
    }
}

// ── The view's own settings ─────────────────────────────────────────────────

/// Fit pane `pane`'s board to what it shows.
pub(crate) fn fit(store: AppStore, pane: PaneId) {
    with_session(pane, |s| s.fitted = false);
    after_change(store, pane);
}

/// Show pane `pane`'s board at one board unit to a CSS pixel.
pub(crate) fn actual_size(store: AppStore, pane: PaneId) {
    let scale = scale_of(pane);
    with_session(pane, |s| {
        let center = s.view.center();
        s.view.set_camera(center, scale);
        s.view.settle(&picture_source());
    });
    after_change(store, pane);
}

/// Show board rectangle `rect` in pane `pane` (a frame from the navigator).
pub(crate) fn show_rect(store: AppStore, pane: PaneId, rect: Rect) {
    let scale = scale_of(pane);
    with_session(pane, |s| {
        s.view.fit(&rect, 32.0 * scale);
        s.view.settle(&picture_source());
    });
    after_change(store, pane);
}

/// The floating markup control (contract section 5): show or hide every
/// mark on the board, for everyone.
pub(crate) fn toggle_markup(store: AppStore, pane: PaneId) {
    let Some(visible) = with_session(pane, |s| s.view.snapshot().markup_visible) else { return };
    new_undo_step(pane);
    local_edit(store, pane, |t| t.set_markup_visible(!visible));
}

/// The theme changed: every board's background follows.
pub(crate) fn set_dark_mode(dark: bool) {
    for pane in PaneId::ALL {
        if with_session(pane, |s| s.view.set_background(background(dark))).is_some() {
            present(pane);
        }
    }
}

/// The grid shown or hidden (per viewer): every board follows.
pub(crate) fn set_grid(store: AppStore, shown: bool) {
    for pane in PaneId::ALL {
        if with_session(pane, |s| s.view.set_grid(shown.then_some(GRID))).is_some() {
            after_change(store, pane);
        }
    }
}

/// "Color by author" in pane `pane` (contract section 6): a way of looking
/// that hides nothing, this pane only.
pub(crate) fn set_color_by_author(store: AppStore, pane: PaneId, on: bool) {
    store.pane(pane).board_ui.by_author.set(on);
    let changed = with_session(pane, |s| {
        if s.options.color_by_author == on {
            return false;
        }
        s.options.color_by_author = on;
        let options = s.options.clone();
        s.view.set_options(options, &s.board);
        true
    });
    if changed == Some(true) {
        after_change(store, pane);
    }
}

/// Node cards' live titles and colors (contract section 4): the view is
/// re-read only when they changed.
fn set_card_labels(store: AppStore, pane: PaneId, labels: HashMap<String, CardLabel>) {
    let changed = with_session(pane, |s| {
        if *s.options.card_labels == labels {
            return false;
        }
        s.options.card_labels = Arc::new(labels);
        let options = s.options.clone();
        s.view.set_options(options, &s.board);
        true
    });
    if changed == Some(true) {
        present(pane);
        let _ = store;
    }
}

/// Pane `pane`'s surface, made once and wired to its board.
pub(crate) fn surface(store: AppStore, pane: PaneId) -> RenderSurfaceHandle {
    if let Some(surface) = surface_of(pane) {
        return surface;
    }
    APP.with(|app| app.set(Some(store)));
    let surface = create_render_surface();
    surface.set_render_callback(move |_writer, _width, _height| present(pane));
    // Pointer events: which device (pen, its eraser, a finger) and how hard,
    // two-finger and Ctrl+wheel zoom as `Pinch`.
    surface.set_pointer_events(true);
    surface.set_event_handler(move |event| tools::on_event(store, pane, event));
    surface.set_key_handler(move |key| tools::claims_key(pane, key));
    SURFACES.with(|s| s.borrow_mut()[pane.index()] = Some(surface.clone()));
    surface
}

// ── Making board nodes ──────────────────────────────────────────────────────

/// The update that makes a new board: one layer to draw in and, for an image
/// node, the picture at one unit per pixel in a locked layer of its own below
/// it (contract sections 1 and 3).
pub(crate) fn new_board(picture: Option<(&BlobUrl, u32, u32)>) -> Option<Vec<u8>> {
    let board = Board::standalone();
    let author = author();
    let now = now_stamp();
    let edited = board.edit(Stamp::new(author.as_ref(), &now), |t| {
        t.create()?;
        if let Some((url, w, h)) = picture {
            let layer = t.add_layer("Picture", Placement::Bottom)?;
            let geom = PictureGeom { rect: Rect::new(0.0, 0.0, w as f32, h as f32), src: url.to_string(), px_w: w, px_h: h, crop: None };
            t.add_element(NewElement::new(Kind::Picture, Geom::Picture(geom)).in_layer(layer.clone()))?;
            t.set_layer_locked(&layer, true)?;
        }
        Ok(())
    });
    match edited.result {
        Ok(()) => edited.update,
        Err(e) => {
            tracing::warn!("a new board could not be made: {e}");
            None
        }
    }
}

/// Make a board node under `parent_id`, its board written in the edit that
/// creates it (two devices creating one board would keep only one).
pub(crate) fn create_board_node(store: AppStore, store_id: StoreId, parent_id: NodeId, title: &str, icon: &str, board: Vec<u8>, request: Option<u64>) {
    store.send(BackendCommand::CreateNode {
        store_id,
        parent_id: Some(parent_id),
        title: title.to_string(),
        init: Some(NodeInit {
            node_type: node_types::BOARD.to_string(),
            content: board,
            custom: vec![
                (pimble_core::custom_keys::ICON.to_string(), serde_json::json!(icon)),
                (pimble_core::custom_keys::EXPLICIT_TITLE.to_string(), serde_json::json!(true)),
            ],
            request,
        }),
    });
}

/// A row's "New Whiteboard": an empty board under `parent_id`.
pub(crate) fn new_whiteboard(store: AppStore, store_id: StoreId, parent_id: NodeId) {
    if let Some(board) = new_board(None) {
        create_board_node(store, store_id, parent_id, "Whiteboard", WHITEBOARD_ICON, board, None);
    }
}

/// A picture on its way to becoming an image node.
struct QueuedImage {
    store_id: StoreId,
    parent_id: NodeId,
    title: String,
    bytes: Vec<u8>,
}

thread_local! {
    /// Image nodes to make, in order: one picture stored at a time, so the
    /// nodes appear in the order the files came.
    static IMAGE_QUEUE: RefCell<VecDeque<QueuedImage>> = const { RefCell::new(VecDeque::new()) };
    /// The picture being stored now: its request id, where it goes, its title.
    static IN_FLIGHT: RefCell<Option<(u64, StoreId, NodeId, String)>> = const { RefCell::new(None) };
    static NEXT_REQUEST: Cell<u64> = const { Cell::new(REQUEST_BASE) };
}

/// The title an image node takes: its file's name without the extension, or
/// "Picture" for a pasted bitmap.
fn image_title(name: Option<&str>) -> String {
    let Some(name) = name.map(|n| n.rsplit(['/', '\\']).next().unwrap_or(n)) else { return "Picture".to_string() };
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    };
    if stem.is_empty() {
        "Picture".to_string()
    } else {
        stem.to_string()
    }
}

/// Make an image node per picture under `parent_id`, in order: dropped
/// files, "New Image...", a paste on a tree row. Each picture is fitted and
/// stored as text pictures are (`PutBlob`: scaled to fit, refused with a
/// sentence), then becomes a `picture` element at one unit per pixel.
pub(crate) fn add_image_nodes(store: AppStore, store_id: StoreId, parent_id: NodeId, pictures: Vec<(Option<String>, Vec<u8>)>) {
    if !store.node_access(store_id, parent_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    IMAGE_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        for (name, bytes) in pictures {
            q.push_back(QueuedImage { store_id, parent_id, title: image_title(name.as_deref()), bytes });
        }
    });
    pump(store);
}

fn pump(store: AppStore) {
    if IN_FLIGHT.with(|f| f.borrow().is_some()) {
        return;
    }
    let Some(next) = IMAGE_QUEUE.with(|q| q.borrow_mut().pop_front()) else { return };
    let request_id = NEXT_REQUEST.with(|n| n.replace(n.get() + 1));
    IN_FLIGHT.with(|f| *f.borrow_mut() = Some((request_id, next.store_id, next.parent_id, next.title)));
    // The picture is stored as a write of the node it goes under, which is
    // what making a child there is.
    store.send(BackendCommand::PutBlob { store_id: next.store_id, node_id: next.parent_id, bytes: next.bytes, request_id });
}

/// The answer to an image node's `PutBlob`; answers whether it was one.
pub(crate) fn stored(store: AppStore, request_id: u64, result: &Result<StoredBlob, String>) -> bool {
    let Some((_, store_id, parent_id, title)) = IN_FLIGHT.with(|f| f.borrow_mut().take_if(|(id, ..)| *id == request_id)) else {
        return false;
    };
    match result {
        Ok(blob) => match blob.size {
            Some((w, h)) if w > 0 && h > 0 => {
                if let Some(board) = new_board(Some((&blob.url, w, h))) {
                    create_board_node(store, store_id, parent_id, &title, PHOTO_ICON, board, None);
                }
                if let Some(notice) = &blob.notice {
                    crate::events::show_notice(store, notice.clone());
                }
            }
            _ => crate::events::show_notice(store, NOT_A_PICTURE.to_string()),
        },
        Err(sentence) => crate::events::show_notice(store, sentence.clone()),
    }
    pump(store);
    true
}

/// A row's "New Image...": pick pictures and make an image node of each.
pub(crate) fn pick_images(store: AppStore, store_id: StoreId, parent_id: NodeId) {
    #[cfg(feature = "native")]
    {
        // Driving the app over rinch's debug port cannot work a file dialog:
        // this names files to take instead (`:`-separated). Never set in
        // ordinary use.
        let paths: Vec<std::path::PathBuf> = match std::env::var("PIMBLE_DEBUG_IMAGES") {
            Ok(list) => std::env::split_paths(&list).collect(),
            Err(_) => rinch::dialogs::open_file()
                .set_title("New Image")
                .add_filter("Pictures", &["png", "jpg", "jpeg", "gif", "webp"])
                .pick_files()
                .unwrap_or_default(),
        };
        add_image_files(store, store_id, parent_id, paths);
    }
    #[cfg(not(feature = "native"))]
    pick_in_browser(store, store_id, parent_id);
}

/// Files dropped on a tree row, or picked: an image node per picture, in
/// order. Files that are not pictures are left out with a sentence.
#[cfg(feature = "native")]
pub(crate) fn add_image_files(store: AppStore, store_id: StoreId, parent_id: NodeId, paths: Vec<std::path::PathBuf>) {
    let mut pictures = Vec::new();
    let mut skipped = 0;
    for path in paths {
        match std::fs::read(&path) {
            Ok(bytes) if pimble_core::ImageMime::sniff(&bytes).is_some() => {
                pictures.push((path.file_name().map(|n| n.to_string_lossy().to_string()), bytes));
            }
            Ok(_) => skipped += 1,
            Err(e) => crate::events::show_notice(store, format!("\"{}\" could not be read: {e}.", path.display())),
        }
    }
    if skipped > 0 {
        crate::events::show_notice(store, NOT_A_PICTURE.to_string());
    }
    if !pictures.is_empty() {
        add_image_nodes(store, store_id, parent_id, pictures);
    }
}

/// A row's "Paste Image" on the desktop: the picture on the clipboard
/// becomes an image node under the row.
#[cfg(feature = "native")]
pub(crate) fn paste_image(store: AppStore, store_id: StoreId, parent_id: NodeId) {
    let image = match rinch::clipboard::paste_image() {
        Ok(image) => image,
        Err(_) => {
            crate::events::show_notice(store, "There is no picture on the clipboard.".to_string());
            return;
        }
    };
    let (w, h) = (image.width as u32, image.height as u32);
    let mut png = Vec::new();
    let encoded = ::image::ImageEncoder::write_image(
        ::image::codecs::png::PngEncoder::new(&mut png),
        &image.bytes,
        w,
        h,
        ::image::ExtendedColorType::Rgba8,
    );
    if encoded.is_err() {
        crate::events::show_notice(store, NOT_A_PICTURE.to_string());
        return;
    }
    add_image_nodes(store, store_id, parent_id, vec![(None, png)]);
}

/// The browser's picker for "New Image...": a hidden file input taking
/// several pictures, read in order.
#[cfg(not(feature = "native"))]
fn pick_in_browser(store: AppStore, store_id: StoreId, parent_id: NodeId) {
    use wasm_bindgen::JsCast;

    const INPUT_ID: &str = "pimble-image-node-input";
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    if let Some(old) = document.get_element_by_id(INPUT_ID) {
        old.remove();
    }
    let Some(input) = document.create_element("input").ok().and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok()) else {
        return;
    };
    input.set_id(INPUT_ID);
    input.set_type("file");
    input.set_multiple(true);
    input.set_accept("image/png,image/jpeg,image/gif,image/webp");
    input.set_hidden(true);
    let Some(body) = document.body() else { return };
    if body.append_child(&input).is_err() {
        return;
    }
    let picked = input.clone();
    // Taken once: a browser may report the change twice, and a one-shot
    // closure called again throws.
    let taken = std::rc::Rc::new(Cell::new(false));
    let on_change = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
        if taken.replace(true) {
            return;
        }
        let files = picked.files();
        picked.remove();
        if let Some(files) = files {
            read_files(store, store_id, parent_id, files, false);
        }
    });
    if input.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref()).is_err() {
        return;
    }
    on_change.forget();
    input.click();
}

/// Read a browser `FileList` in order and make an image node of each picture.
/// `pasted`: a clipboard bitmap, which the browser names `image.png`; the
/// node is called "Picture" instead.
#[cfg(not(feature = "native"))]
fn read_files(store: AppStore, store_id: StoreId, parent_id: NodeId, files: web_sys::FileList, pasted: bool) {
    let files: Vec<web_sys::File> = (0..files.length()).filter_map(|i| files.get(i)).collect();
    if files.is_empty() {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        let mut pictures = Vec::new();
        for file in files {
            let name = file.name();
            match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => {
                    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                    if pimble_core::ImageMime::sniff(&bytes).is_some() {
                        pictures.push(((!pasted).then_some(name), bytes));
                    } else {
                        crate::events::show_notice(store, NOT_A_PICTURE.to_string());
                    }
                }
                Err(_) => crate::events::show_notice(store, format!("\"{name}\" could not be read.")),
            }
        }
        if !pictures.is_empty() {
            add_image_nodes(store, store_id, parent_id, pictures);
        }
    });
}

/// The browser's way in for a drop of image files on a tree row and a paste
/// with a tree row focused: page-level listeners, installed once, that find
/// the row by its `data-pimble-row` attribute (dropped) or the tree's
/// selected row (pasted while no editor or field has the keyboard).
#[cfg(not(feature = "native"))]
pub(crate) fn install_browser_handlers(store: AppStore) {
    use wasm_bindgen::JsCast;

    thread_local! {
        static INSTALLED: Cell<bool> = const { Cell::new(false) };
    }
    if INSTALLED.with(|i| i.replace(true)) {
        return;
    }
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };

    fn row_of(target: Option<web_sys::EventTarget>) -> Option<String> {
        let element = target?.dyn_into::<web_sys::Element>().ok()?;
        element.closest("[data-pimble-row]").ok()??.get_attribute("data-pimble-row")
    }
    fn carries_files(event: &web_sys::DragEvent) -> bool {
        event.data_transfer().is_some_and(|dt| dt.types().iter().any(|t| t.as_string().as_deref() == Some("Files")))
    }

    let over = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::DragEvent)>::new(move |event: web_sys::DragEvent| {
        if carries_files(&event) && row_of(event.target()).is_some() {
            event.prevent_default();
        }
    });
    let _ = document.add_event_listener_with_callback("dragover", over.as_ref().unchecked_ref());
    over.forget();

    let drop = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::DragEvent)>::new(move |event: web_sys::DragEvent| {
        if !carries_files(&event) {
            return;
        }
        let Some(row) = row_of(event.target()) else { return };
        event.prevent_default();
        let Some(files) = event.data_transfer().and_then(|dt| dt.files()) else { return };
        if let Some((store_id, parent_id)) = parent_for_row(store, &row) {
            read_files(store, store_id, parent_id, files, false);
        }
    });
    let _ = document.add_event_listener_with_callback("drop", drop.as_ref().unchecked_ref());
    drop.forget();

    let paste = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::ClipboardEvent)>::new(move |event: web_sys::ClipboardEvent| {
        if event.default_prevented() {
            return;
        }
        // Only when nothing that takes text has the keyboard.
        let focused = web_sys::window().and_then(|w| w.document()).and_then(|d| d.active_element());
        if focused.is_some_and(|el| {
            let tag = el.tag_name().to_ascii_lowercase();
            tag == "input" || tag == "textarea" || el.closest("[data-pm-editor]").ok().flatten().is_some() || el.closest("[contenteditable]").ok().flatten().is_some()
        }) {
            return;
        }
        if untracked(|| store.pane(store.focused()).show_editor.get()) {
            return;
        }
        let Some(files) = event.clipboard_data().and_then(|dt| dt.files()) else { return };
        if files.length() == 0 {
            return;
        }
        let Some(row) = untracked(|| store.selected_id.get()) else { return };
        let Some((store_id, parent_id)) = parent_for_row(store, &row) else { return };
        event.prevent_default();
        read_files(store, store_id, parent_id, files, true);
    });
    let _ = document.add_event_listener_with_callback("paste", paste.as_ref().unchecked_ref());
    paste.forget();
}

/// Where a new node made "on" a tree row goes: the row's node, a store row's
/// root, or a mount's source node (a mount node takes no children).
pub(crate) fn parent_for_row(store: AppStore, row: &str) -> Option<(StoreId, NodeId)> {
    let (store_id, node_id) = crate::state::parse_tree_value(row)?;
    let node_id = node_id.or_else(|| store.root_node_id(store_id))?;
    if store.is_mount(store_id, node_id) {
        let mount_ref = store.get_node_signal(store_id, node_id).and_then(|sig| untracked(|| sig.with(|n| n.mount_ref())))?;
        return Some((mount_ref.source_store, mount_ref.source_node));
    }
    Some((store_id, node_id))
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::protocol::{BackendEvent, BackendHandle};
    use crossbeam_channel::bounded;

    const P: [PaneId; MAX_PANES] = PaneId::ALL;

    fn store_with_commands() -> (AppStore, crossbeam_channel::Receiver<BackendCommand>) {
        let store = AppStore::new();
        let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(256);
        let (_event_tx, event_rx) = bounded::<BackendEvent>(1);
        store.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
        (store, cmd_rx)
    }

    fn whiteboard_bytes() -> Vec<u8> {
        let mut doc = NodeDoc::new();
        doc.init(node_types::BOARD, "Whiteboard", None, "2026-10-09T10:00:00.000Z").unwrap();
        doc.apply_update(&new_board(None).unwrap()).unwrap();
        doc.save()
    }

    fn texts(pane: PaneId) -> usize {
        with_session(pane, |s| s.view.snapshot().elements.len()).unwrap()
    }

    /// The same board in two panes (the editors' rule, docs/SPLIT_VIEW_CONTRACT.md):
    /// an edit in one reaches the other and goes to the server once; a
    /// peer's update reaches both; the node is subscribed once and let go
    /// of when the last pane does.
    #[test]
    fn the_same_board_in_two_panes_stays_in_step() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        let bytes = whiteboard_bytes();
        start(store, P[0], store_id, node_id, &bytes);
        start(store, P[1], store_id, node_id, &bytes);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        let subscribes = sent.iter().filter(|c| matches!(c, BackendCommand::SubscribeNodeChanges { .. })).count();
        assert_eq!(subscribes, 1, "once per node");

        let id = local_edit(store, P[0], |t| t.add_element(NewElement::stroke(Kind::Pen, vec![[0.0, 0.0, 0.5], [10.0, 10.0, 0.5]])));
        assert!(id.is_some());
        assert_eq!(texts(P[0]), 1);
        assert_eq!(texts(P[1]), 1, "handed across");
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(sent.iter().filter(|c| matches!(c, BackendCommand::BroadcastChanges { .. })).count(), 1);

        // A peer's stroke arrives as `RemoteChanges` would bring it.
        let mut peer = NodeDoc::load(&bytes).unwrap();
        let before = peer.state_vector();
        let peer_board = Board::of_node(&peer);
        peer_board
            .add_element(Stamp::new(None, "2026-10-09T10:00:01.000Z"), NewElement::stroke(Kind::Pen, vec![[5.0, 5.0, 0.5]]))
            .unwrap();
        let delta = peer.diff_since(&before).unwrap();
        peer.apply_update(&delta).unwrap();
        apply_remote(store, store_id, node_id, &delta);
        assert_eq!(texts(P[0]), 2);
        assert_eq!(texts(P[1]), 2);

        // Undo is this pane's own: it takes back the first stroke, never the peer's.
        tools::undo(store, P[0], false);
        assert_eq!(texts(P[0]), 1);
        assert_eq!(texts(P[1]), 1);

        stop(store, P[0]);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(!sent.iter().any(|c| matches!(c, BackendCommand::UnsubscribeNodeChanges { .. })));
        stop(store, P[1]);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(sent.iter().any(|c| matches!(c, BackendCommand::UnsubscribeNodeChanges { .. })));
    }

    /// An image node's board: the picture at one unit per pixel in a locked
    /// layer of its own, and a layer above it to draw in.
    #[test]
    fn an_image_board_holds_its_picture_in_a_locked_layer() {
        let url = BlobUrl::parse("pimble-blob:00000000-0000-0000-0000-000000000001/aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let mut doc = NodeDoc::new();
        doc.init(node_types::BOARD, "Shot", None, "2026-10-09T10:00:00.000Z").unwrap();
        doc.apply_update(&new_board(Some((&url, 640, 480))).unwrap()).unwrap();
        let snapshot = Board::of_node(&doc).snapshot();
        assert_eq!(snapshot.layers.len(), 2);
        let picture = &snapshot.elements[0];
        assert!(snapshot.layer(&picture.layer).unwrap().locked);
        assert_eq!(picture.bounds, Rect::new(0.0, 0.0, 640.0, 480.0));
        assert!(!snapshot.top_layer().unwrap().locked, "new marks go above, into a layer that takes them");
        assert_eq!(snapshot.markup_count(), 0);
    }

    #[test]
    fn image_titles_come_from_file_names() {
        assert_eq!(image_title(Some("/tmp/Screenshot 1.png")), "Screenshot 1");
        assert_eq!(image_title(None), "Picture");
        assert_eq!(image_title(Some(".png")), ".png");
    }

    fn rect_element(x: f32) -> NewElement {
        NewElement::new(Kind::Rect, Geom::Rect(Rect::new(x, 0.0, 50.0, 50.0)))
    }

    fn element_count(pane: PaneId) -> usize {
        with_session(pane, |s| s.snapshot().elements.len()).unwrap()
    }

    /// Text is typed into a real input (contract section 4): the first
    /// character makes the element, later ones edit its yrs Text, and a text
    /// left empty goes when the typing ends.
    #[test]
    fn typing_makes_and_edits_a_text_element() {
        let (store, _commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        start(store, P[0], store_id, node_id, &whiteboard_bytes());
        tools::begin_text_new_for_test(store, P[0], Point::new(10.0, 10.0));
        assert_eq!(element_count(P[0]), 0, "nothing until a character is typed");
        tools::text_input(store, P[0], "Hi".to_string());
        tools::text_input(store, P[0], "Hi there".to_string());
        let text = with_session(P[0], |s| s.snapshot().elements[0].raw.text.clone()).unwrap();
        assert_eq!(text.as_deref(), Some("Hi there"));
        tools::text_input(store, P[0], String::new());
        tools::end_text(store, P[0]);
        assert_eq!(element_count(P[0]), 0, "an emptied text goes");
        stop(store, P[0]);
    }

    /// Groups (contract section 4): grouping makes one element the members
    /// name as parent; a duplicate copies the members too; ungrouping lets
    /// them go where they were; deleting a group deletes its members.
    #[test]
    fn grouping_duplicating_and_ungrouping() {
        let (store, _commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        start(store, P[0], store_id, node_id, &whiteboard_bytes());
        let a = local_edit(store, P[0], |t| t.add_element(rect_element(0.0))).unwrap();
        let b = local_edit(store, P[0], |t| t.add_element(rect_element(100.0))).unwrap();
        tools::select(store, P[0], vec![a.clone(), b.clone()]);
        tools::group(store, P[0]);
        let group = with_session(P[0], |s| s.selected.clone()).unwrap();
        assert_eq!(group.len(), 1);
        let parents = with_session(P[0], |s| [s.snapshot().get(&a).unwrap().parent.clone(), s.snapshot().get(&b).unwrap().parent.clone()]).unwrap();
        assert_eq!(parents, [Some(group[0].clone()), Some(group[0].clone())]);

        tools::duplicate(store, P[0]);
        assert_eq!(element_count(P[0]), 6, "a group and two members, twice");
        let copy = with_session(P[0], |s| s.selected.clone()).unwrap();
        assert_ne!(copy, group);
        tools::delete_selected(store, P[0]);
        assert_eq!(element_count(P[0]), 3, "the copy went with its members");

        tools::select(store, P[0], group.clone());
        tools::ungroup(store, P[0]);
        assert_eq!(element_count(P[0]), 2);
        let (pa, bounds) = with_session(P[0], |s| (s.snapshot().get(&a).unwrap().parent.clone(), s.snapshot().get(&b).unwrap().bounds)).unwrap();
        assert_eq!(pa, None);
        assert_eq!(bounds.x, 98.0, "it stayed where it was (its outline included)");
        stop(store, P[0]);
    }

    /// New elements go into the picked layer, and a locked one refuses them.
    #[test]
    fn a_locked_layer_takes_no_new_elements() {
        let (store, _commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        start(store, P[0], store_id, node_id, &whiteboard_bytes());
        let layer = with_session(P[0], |s| s.snapshot().layers[0].id.clone()).unwrap();
        local_edit(store, P[0], |t| t.set_layer_locked(&layer, true));
        tools::begin_text_new_for_test(store, P[0], Point::new(0.0, 0.0));
        tools::text_input(store, P[0], "x".to_string());
        assert_eq!(element_count(P[0]), 0);
        stop(store, P[0]);
    }

    fn pen(id: u64, pressure: f32) -> rinch::render_surface::SurfacePointer {
        rinch::render_surface::SurfacePointer { id, kind: rinch::render_surface::SurfacePointerKind::Pen, pressure, primary: true }
    }

    fn finger(id: u64) -> rinch::render_surface::SurfacePointer {
        rinch::render_surface::SurfacePointer { id, kind: rinch::render_surface::SurfacePointerKind::Touch, pressure: 0.5, primary: id == 10 }
    }

    fn broadcasts(commands: &crossbeam_channel::Receiver<BackendCommand>) -> usize {
        commands.try_iter().filter(|c| matches!(c, BackendCommand::BroadcastChanges { .. })).count()
    }

    /// Pointer events (rinch's `set_pointer_events`): a pen's pressure is
    /// stored with each point, a mouse's is 0.5; a cancelled stroke and a
    /// finger that turns out to be the first of a pinch write nothing; the
    /// pen's eraser end erases whatever the tool; one gesture at a time.
    #[test]
    fn pointers_draw_with_their_pressure_and_cancelled_strokes_leave_nothing() {
        use rinch::render_surface::{SurfacePointer, SurfacePointerKind};
        let left = SurfaceMouseButton::Left;
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        start(store, P[0], store_id, node_id, &whiteboard_bytes());
        store.board_tool.set(Tool::Pen);
        let _ = broadcasts(&commands);

        // A pen pressed harder as it goes.
        tools::pointer_down(store, P[0], 0.0, 0.0, left, pen(7, 0.2));
        for i in 1..=10 {
            tools::pointer_move(store, P[0], i as f32 * 8.0, 0.0, pen(7, 0.2 + i as f32 * 0.07));
        }
        // A second pointer's press meanwhile starts nothing.
        tools::pointer_down(store, P[0], 50.0, 50.0, left, SurfacePointer::mouse(true));
        tools::pointer_up(store, P[0], 50.0, 50.0, left, SurfacePointer::mouse(false));
        tools::pointer_up(store, P[0], 80.0, 0.0, left, pen(7, 0.0));
        assert_eq!(element_count(P[0]), 1);
        let pressures: Vec<f32> = with_session(P[0], |s| s.snapshot().elements[0].raw.points.iter().map(|p| p[2]).collect()).unwrap();
        assert!(pressures.first().unwrap() < &0.35 && pressures.last().unwrap() > &0.8, "{pressures:?}");
        assert_eq!(broadcasts(&commands), 1, "written once, whole, on release");

        // A mouse draws at 0.5, whatever it reports.
        let mut mouse = SurfacePointer::mouse(true);
        mouse.pressure = 0.9;
        tools::pointer_down(store, P[0], 0.0, 40.0, left, mouse);
        tools::pointer_move(store, P[0], 30.0, 40.0, mouse);
        tools::pointer_up(store, P[0], 30.0, 40.0, left, SurfacePointer::mouse(false));
        assert_eq!(element_count(P[0]), 2);
        let all_half = with_session(P[0], |s| s.snapshot().elements[1].raw.points.iter().all(|p| p[2] == 0.5)).unwrap();
        assert!(all_half);
        let _ = broadcasts(&commands);

        // A stroke the platform cancels is gone, and nothing was written.
        tools::pointer_down(store, P[0], 0.0, 80.0, left, pen(7, 0.5));
        tools::pointer_move(store, P[0], 40.0, 80.0, pen(7, 0.6));
        tools::pointer_cancel(store, P[0], pen(7, 0.0));
        assert_eq!(element_count(P[0]), 2);
        assert_eq!(broadcasts(&commands), 0);
        assert!(with_session(P[0], |s| matches!(s.gesture, Gesture::Idle) && s.driver.is_none()).unwrap());

        // One finger starts a stroke; a second makes it a pinch: the stroke
        // goes unwritten, and moving the fingers pans.
        tools::pointer_down(store, P[0], 0.0, 120.0, left, finger(10));
        tools::pointer_move(store, P[0], 10.0, 120.0, finger(10));
        tools::pointer_down(store, P[0], 100.0, 120.0, left, finger(11));
        assert!(with_session(P[0], |s| matches!(s.gesture, Gesture::Pinch)).unwrap());
        let before = with_session(P[0], |s| s.view.screen_to_board(0.0, 0.0)).unwrap();
        tools::pointer_move(store, P[0], 30.0, 140.0, finger(10));
        tools::pointer_move(store, P[0], 120.0, 140.0, finger(11));
        let after = with_session(P[0], |s| s.view.screen_to_board(0.0, 0.0)).unwrap();
        let zoom = with_session(P[0], |s| s.view.zoom()).unwrap();
        assert!(((before.x - after.x) * zoom - 20.0).abs() < 1e-3 && ((before.y - after.y) * zoom - 20.0).abs() < 1e-3, "{before:?} {after:?}");
        tools::pointer_up(store, P[0], 30.0, 140.0, left, finger(10));
        tools::pointer_move(store, P[0], 200.0, 200.0, finger(11));
        tools::pointer_up(store, P[0], 200.0, 200.0, left, finger(11));
        assert_eq!(element_count(P[0]), 2);
        assert_eq!(broadcasts(&commands), 0);
        assert!(with_session(P[0], |s| matches!(s.gesture, Gesture::Idle) && s.touches.is_empty()).unwrap());

        // The pen's eraser end erases with the pen tool still chosen.
        let on_first = with_session(P[0], |s| {
            let e = &s.snapshot().elements[0];
            s.view.board_to_screen(Point::new(e.raw.points[3][0], e.raw.points[3][1]))
        })
        .unwrap();
        let eraser = SurfacePointer { kind: SurfacePointerKind::Eraser, ..pen(8, 0.5) };
        tools::pointer_down(store, P[0], on_first.0, on_first.1, left, eraser);
        tools::pointer_up(store, P[0], on_first.0, on_first.1, left, eraser);
        assert_eq!(element_count(P[0]), 1);
        assert_eq!(untracked(|| store.board_tool.get()), Tool::Pen);
        stop(store, P[0]);
    }
}
