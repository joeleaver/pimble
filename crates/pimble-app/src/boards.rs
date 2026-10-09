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
//! Pictures on a board come from the same blob cache text pictures do
//! (`GetBlob`/`BlobLoaded`), decoded off the UI thread on the desktop.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use pimble_board::{
    smooth_stroke, Author, Board, BoardTxn, BoardView, BoardWatch, Color, Geom, Kind, NewElement, Picture, PictureGeom, Placement,
    Point, Rect, Stamp, Style, Transform,
};
use pimble_core::{node_types, BlobUrl, NodeId, StoreId};
use pimble_crdt::NodeDoc;
use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use crate::appearance::IconGlyph;
use crate::panes::{PaneId, MAX_PANES};
use crate::protocol::{BackendCommand, NodeInit, StoredBlob};
use crate::state::{ActiveEdit, AppStore};

// ── Tools ───────────────────────────────────────────────────────────────────

/// The board tools of wave 1 (contract section 4): select (click, drag to
/// move, Delete) and the pen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Select,
    Pen,
}

/// The pen's first color: the marks' red.
pub const DEFAULT_COLOR: &str = "#e53935";
/// The pen's first width, in board units.
pub const DEFAULT_WIDTH: f32 = 4.0;
/// The pen colors the toolbar offers.
const COLORS: [&str; 6] = ["#e53935", "#fb8c00", "#fdd835", "#43a047", "#1e88e5", "#212121"];
/// The pen widths the toolbar offers (thin, medium, thick).
const WIDTHS: [f32; 3] = [2.0, 4.0, 8.0];

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
fn now_stamp() -> String {
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

/// What the pointer is doing on a board.
enum Gesture {
    Idle,
    /// Middle-drag or space-drag: the last position, physical pixels.
    Pan { last: (f32, f32) },
    /// A pen stroke: its element, and the points not yet appended.
    Stroke { id: String, pending: Vec<[f32; 3]>, appended_at: f64 },
    /// Moving an element: where the drag began (board units), the element's
    /// transform then, and the last position written.
    Drag { id: String, start: Point, base: Transform, last: Point, written_at: f64 },
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
    selected: Option<String>,
    /// The space bar is held: a left drag pans.
    space: bool,
    /// Bumped by every wheel step; the settle timer of an older one does nothing.
    settle_generation: u64,
}

impl Session {
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
                    self.view.set_snapshot(self.board.snapshot());
                }
            }
        }
        if self.undo.is_none() {
            self.undo = self.board.undo_manager(undo_options());
        }
        if let Some(id) = &self.selected {
            if self.view.snapshot().get(id).is_none() {
                self.selected = None;
            }
        }
    }

    /// The selected element's rectangle on screen, in physical pixels.
    fn selection_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let e = self.view.snapshot().get(self.selected.as_deref()?)?;
        let (x0, y0) = self.view.board_to_screen(Point::new(e.bounds.x, e.bounds.y));
        let (x1, y1) = self.view.board_to_screen(Point::new(e.bounds.right(), e.bounds.bottom()));
        Some((x0, y0, x1 - x0, y1 - y0))
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
    let mut session = Session {
        store_id,
        node_id,
        doc,
        board,
        watch: None,
        undo: None,
        view: BoardView::new(1, 1, background(dark)),
        fitted: false,
        gesture: Gesture::Idle,
        selected: None,
        space: false,
        settle_generation: 0,
    };
    session.sync_view();
    SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()] = Some(session));
    let state = store.pane(pane);
    state.board.set(Some(ActiveEdit { store_id, node_id }));
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
    let Some(session) = SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()].take()) else { return };
    if let Some(sig) = store.get_node_signal(session.store_id, session.node_id) {
        let bytes = session.doc.save();
        sig.update(|node| node.content = bytes);
    }
    let state = store.pane(pane);
    state.board.set(None);
    state.board_selection.set(None);
    state.board_marks.set((0, true));
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

/// One local edit of pane `pane`'s board, stamped with who and when: refused
/// (with the sentence) when this device may only read the node. Answers the
/// closure's result.
fn local_edit<R>(store: AppStore, pane: PaneId, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> pimble_board::doc::Result<R>) -> Option<R> {
    let held = with_session(pane, |s| (s.store_id, s.node_id))?;
    if !store.node_access(held.0, held.1).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return None;
    }
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

/// After the pane's board or view changed: the floating controls follow,
/// then the surface is handed a frame if one is due.
fn after_change(store: AppStore, pane: PaneId) {
    let scale = scale_of(pane);
    let ui = with_session(pane, |s| {
        let snapshot = s.view.snapshot();
        let marks = (snapshot.markup_count(), snapshot.markup_visible);
        let selection = s.selection_rect().map(|(x, y, w, h)| (x / scale, y / scale, w / scale, h / scale));
        (marks, selection)
    });
    let state = store.pane(pane);
    match ui {
        Some((marks, selection)) => {
            state.board_marks.set_if_changed(marks);
            state.board_selection.set_if_changed(selection);
        }
        None => {
            state.board_selection.set_if_changed(None);
        }
    }
    present(pane);
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

// ── Pictures on boards ──────────────────────────────────────────────────────

#[derive(Default)]
struct Pictures {
    /// Decoded, by `src`.
    decoded: HashMap<String, Arc<Picture>>,
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
        Ok(picture) => PICTURES.with(|p| {
            p.borrow_mut().decoded.insert(src.clone(), picture);
        }),
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
            !rects.is_empty()
        });
        if touched == Some(true) {
            present(pane);
        }
    }
}

// ── Pointer and keys ────────────────────────────────────────────────────────

/// Pane `pane`'s surface, made once and wired to its board.
pub(crate) fn surface(store: AppStore, pane: PaneId) -> RenderSurfaceHandle {
    if let Some(surface) = surface_of(pane) {
        return surface;
    }
    APP.with(|app| app.set(Some(store)));
    let surface = create_render_surface();
    surface.set_render_callback(move |_writer, _width, _height| present(pane));
    surface.set_event_handler(move |event| on_event(store, pane, event));
    surface.set_key_handler(move |key| claims_key(pane, key));
    SURFACES.with(|s| s.borrow_mut()[pane.index()] = Some(surface.clone()));
    surface
}

/// Which keys a focused board keeps from the rest of the app.
fn claims_key(pane: PaneId, key: &SurfaceKeyData) -> bool {
    let primary = key.ctrl || key.meta;
    match key.key.as_str() {
        " " => true,
        "Delete" | "Backspace" => with_session(pane, |s| s.selected.is_some()).unwrap_or(false),
        "z" | "Z" | "y" | "Y" => primary,
        "Escape" => with_session(pane, |s| s.selected.is_some() || !matches!(s.gesture, Gesture::Idle)).unwrap_or(false),
        _ => false,
    }
}

fn wheel_zooms_out(delta_y: f32) -> bool {
    // A wheel turned toward the person: negative on the desktop (winit's
    // sign), positive in the browser (`WheelEvent.deltaY`).
    if cfg!(feature = "native") {
        delta_y < 0.0
    } else {
        delta_y > 0.0
    }
}

fn on_event(store: AppStore, pane: PaneId, event: SurfaceEvent) {
    if with_session(pane, |_| ()).is_none() {
        return;
    }
    let scale = scale_of(pane);
    match event {
        SurfaceEvent::MouseDown { x, y, button } => {
            let (px, py) = (x * scale, y * scale);
            // A press on the board focuses its pane, as a press in an editor does.
            crate::pane_view::focus_pane(store, pane);
            crate::editor::give_keyboard_to(store, pane);
            let space = with_session(pane, |s| s.space).unwrap_or(false);
            match button {
                SurfaceMouseButton::Middle => begin_pan(pane, px, py),
                SurfaceMouseButton::Left if space => begin_pan(pane, px, py),
                SurfaceMouseButton::Left => match untracked(|| store.board_tool.get()) {
                    Tool::Pen => begin_stroke(store, pane, px, py),
                    Tool::Select => begin_select(store, pane, px, py),
                },
                SurfaceMouseButton::Right => {}
            }
        }
        SurfaceEvent::MouseMove { x, y } => pointer_moved(store, pane, x * scale, y * scale),
        SurfaceEvent::MouseUp { x, y, .. } => pointer_released(store, pane, x * scale, y * scale),
        SurfaceEvent::MouseWheel { x, y, delta_y, .. } => {
            if delta_y == 0.0 {
                return;
            }
            let factor = if wheel_zooms_out(delta_y) { 1.0 / 1.1 } else { 1.1 };
            let generation = with_session(pane, |s| {
                s.view.zoom_at(x * scale, y * scale, factor);
                s.settle_generation += 1;
                s.settle_generation
            });
            after_change(store, pane);
            if let Some(generation) = generation {
                let _ = set_timeout(SETTLE_MS, move || {
                    let settled = with_session(pane, |s| s.settle_generation == generation && s.view.settle(&picture_source()));
                    if settled == Some(true) {
                        present(pane);
                    }
                });
            }
        }
        SurfaceEvent::KeyDown(key) => key_down(store, pane, &key),
        SurfaceEvent::KeyUp(key) => {
            if key.key == " " {
                with_session(pane, |s| s.space = false);
            }
        }
        SurfaceEvent::FocusLost | SurfaceEvent::MouseLeave => {
            with_session(pane, |s| s.space = false);
        }
        _ => {}
    }
}

fn begin_pan(pane: PaneId, px: f32, py: f32) {
    with_session(pane, |s| s.gesture = Gesture::Pan { last: (px, py) });
}

fn begin_stroke(store: AppStore, pane: PaneId, px: f32, py: f32) {
    let Some(held) = with_session(pane, |s| (s.store_id, s.node_id)) else { return };
    if !store.node_access(held.0, held.1).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    let style = Style {
        stroke: Color::parse(&untracked(|| store.board_color.get())),
        width: Some(untracked(|| store.board_width.get())),
        ..Style::default()
    };
    let point = with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        let point = [p.x, p.y, 0.5];
        s.view.begin_stroke(Kind::Pen, &style, point);
        // A new stroke is a new undo step, however soon after the last.
        if let Some(undo) = s.undo.as_mut() {
            undo.reset();
        }
        point
    });
    let Some(point) = point else { return };
    let id = local_edit(store, pane, |t| t.add_element(NewElement::stroke(Kind::Pen, vec![point]).with_style(style)));
    with_session(pane, |s| match id {
        Some(id) => {
            s.selected = None;
            s.gesture = Gesture::Stroke { id, pending: Vec::new(), appended_at: now_ms() };
        }
        None => s.view.cancel_stroke(),
    });
    after_change(store, pane);
}

fn begin_select(store: AppStore, pane: PaneId, px: f32, py: f32) {
    with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        let snapshot = s.view.snapshot();
        let hit = snapshot.hit(p).filter(|e| {
            let layer_locked = snapshot.layer(&e.layer).is_some_and(|l| l.locked);
            !e.raw.locked && !layer_locked
        });
        match hit {
            Some(e) => {
                let id = e.id().to_string();
                s.gesture = Gesture::Drag { id: id.clone(), start: p, base: e.raw.transform, last: p, written_at: 0.0 };
                s.selected = Some(id);
            }
            None => {
                s.selected = None;
                s.gesture = Gesture::Idle;
            }
        }
    });
    after_change(store, pane);
}

fn pointer_moved(store: AppStore, pane: PaneId, px: f32, py: f32) {
    enum Next {
        Nothing,
        Redraw,
        Append(String, Vec<[f32; 3]>),
        Move(String, Transform),
    }
    let writable = with_session(pane, |s| store.node_access(s.store_id, s.node_id).allows_write()).unwrap_or(false);
    let next = with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        match &mut s.gesture {
            Gesture::Idle => Next::Nothing,
            Gesture::Pan { last } => {
                let (dx, dy) = (px - last.0, py - last.1);
                *last = (px, py);
                s.view.pan_by(dx, dy);
                Next::Redraw
            }
            Gesture::Stroke { id, pending, appended_at } => {
                let point = [p.x, p.y, 0.5];
                pending.push(point);
                let id = id.clone();
                let due = now_ms() - *appended_at >= APPEND_EVERY_MS;
                let batch = if due {
                    *appended_at = now_ms();
                    Some(std::mem::take(pending))
                } else {
                    None
                };
                s.view.extend_stroke(&[point]);
                match batch {
                    Some(points) => Next::Append(id, points),
                    None => Next::Redraw,
                }
            }
            Gesture::Drag { id, start, base, last, written_at } => {
                if !writable {
                    return Next::Nothing;
                }
                *last = p;
                if now_ms() - *written_at < DRAG_WRITE_EVERY_MS {
                    return Next::Nothing;
                }
                *written_at = now_ms();
                let moved = Transform { tx: base.tx + p.x - start.x, ty: base.ty + p.y - start.y, ..*base };
                Next::Move(id.clone(), moved)
            }
        }
    });
    match next {
        Some(Next::Redraw) => after_change(store, pane),
        Some(Next::Append(id, points)) => {
            local_edit(store, pane, |t| t.append_points(&id, &points));
        }
        Some(Next::Move(id, transform)) => {
            local_edit(store, pane, |t| t.set_transform(&id, &transform));
        }
        _ => {}
    }
}

fn pointer_released(store: AppStore, pane: PaneId, px: f32, py: f32) {
    let gesture = with_session(pane, |s| std::mem::replace(&mut s.gesture, Gesture::Idle));
    match gesture {
        Some(Gesture::Stroke { id, mut pending, .. }) => {
            let (points, min_gap) = with_session(pane, |s| {
                let p = s.view.screen_to_board(px, py);
                pending.push([p.x, p.y, 0.5]);
                s.view.extend_stroke(&pending[pending.len() - 1..]);
                let mut points = s.view.end_stroke();
                points.push([p.x, p.y, 0.5]);
                (points, 0.75 / s.view.zoom().max(1e-3))
            })
            .unwrap_or_default();
            // Smoothed once, on release, and never edited after.
            let smoothed = smooth_stroke(&points, min_gap);
            local_edit(store, pane, |t| t.replace_points(&id, &smoothed));
        }
        Some(Gesture::Drag { id, start, base, last, .. }) => {
            if (last.x - start.x).abs() > f32::EPSILON || (last.y - start.y).abs() > f32::EPSILON {
                let moved = Transform { tx: base.tx + last.x - start.x, ty: base.ty + last.y - start.y, ..base };
                if with_session(pane, |s| store.node_access(s.store_id, s.node_id).allows_write()) == Some(true) {
                    local_edit(store, pane, |t| t.set_transform(&id, &moved));
                }
            }
            after_change(store, pane);
        }
        _ => after_change(store, pane),
    }
}

fn key_down(store: AppStore, pane: PaneId, key: &SurfaceKeyData) {
    let primary = key.ctrl || key.meta;
    match key.key.as_str() {
        " " => {
            with_session(pane, |s| s.space = true);
        }
        "Delete" | "Backspace" => delete_selected(store, pane),
        "Escape" => {
            with_session(pane, |s| {
                if matches!(s.gesture, Gesture::Stroke { .. }) {
                    s.view.cancel_stroke();
                }
                s.gesture = Gesture::Idle;
                s.selected = None;
            });
            after_change(store, pane);
        }
        "z" | "Z" if primary => undo(store, pane, key.shift),
        "y" | "Y" if primary => undo(store, pane, true),
        _ => {}
    }
}

/// Delete the selected element of pane `pane`'s board.
pub(crate) fn delete_selected(store: AppStore, pane: PaneId) {
    let Some(id) = with_session(pane, |s| s.selected.clone()).flatten() else { return };
    local_edit(store, pane, |t| t.delete_element(&id));
}

/// Undo (or redo) this person's last edit of pane `pane`'s board, never a
/// collaborator's (contract section 4).
pub(crate) fn undo(store: AppStore, pane: PaneId, redo: bool) {
    let Some(held) = with_session(pane, |s| (s.store_id, s.node_id)) else { return };
    if !store.node_access(held.0, held.1).allows_write() {
        return;
    }
    let update = with_session(pane, |s| {
        let update = s.undo.as_mut().and_then(|undo| s.board.undo(undo, redo));
        s.sync_view();
        update
    })
    .flatten();
    if let Some(bytes) = update {
        send_update(store, pane, held.0, held.1, bytes);
    }
    after_change(store, pane);
}

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

/// The floating markup control (contract section 5): show or hide every
/// mark on the board, for everyone.
pub(crate) fn toggle_markup(store: AppStore, pane: PaneId) {
    let Some(visible) = with_session(pane, |s| s.view.snapshot().markup_visible) else { return };
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

// ── Making board nodes ──────────────────────────────────────────────────────

/// The update that makes a new board: one layer to draw in and, for an image
/// node, the picture at one unit per pixel in a locked layer of its own below
/// it (contract sections 1 and 3).
fn new_board(picture: Option<(&BlobUrl, u32, u32)>) -> Option<Vec<u8>> {
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
fn create_board_node(store: AppStore, store_id: StoreId, parent_id: NodeId, title: &str, icon: &str, board: Vec<u8>) {
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
        }),
    });
}

/// A row's "New Whiteboard": an empty board under `parent_id`.
pub(crate) fn new_whiteboard(store: AppStore, store_id: StoreId, parent_id: NodeId) {
    if let Some(board) = new_board(None) {
        create_board_node(store, store_id, parent_id, "Whiteboard", WHITEBOARD_ICON, board);
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
                    create_board_node(store, store_id, parent_id, &title, PHOTO_ICON, board);
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

// ── Drawing the pane ────────────────────────────────────────────────────────

/// Pane `pane`'s board view: the surface, the selection outline over it,
/// and the floating markup control. Rendered once per pane slot.
pub(crate) fn render_board(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    let surface = surface(store, pane);
    let read_only = move || {
        state
            .board
            .get()
            .is_some_and(|held| !store.node_access_tracked(held.store_id, held.node_id).allows_write())
    };
    let view = rsx! {
        div {
            class: "pimble-board",
            style: {move || if state.show_board.get() { "" } else { "display: none;" }},
            RenderSurface { surface: Some(surface) }
            div {
                class: "pimble-board__selection",
                style: {move || match state.board_selection.get() {
                    Some((x, y, w, h)) => format!("left: {x:.1}px; top: {y:.1}px; width: {w:.1}px; height: {h:.1}px;"),
                    None => "display: none;".to_string(),
                }},
            }
            div {
                class: {move || {
                    let (_, visible) = state.board_marks.get();
                    if visible { "pimble-board__markup" } else { "pimble-board__markup pimble-board__markup--hidden" }
                }},
                style: {move || if state.board_marks.get().0 > 0 { "" } else { "display: none;" }},
                title: {move || if read_only() { "You can read this, not change it." } else { "Show or hide the markup, for everyone" }},
                onclick: move || {
                    if !read_only() {
                        toggle_markup(store, pane);
                    }
                },
                span {
                    class: "pimble-board__markup-icon",
                    if state.board_marks.get().1 {
                        IconGlyph { name: TablerIcon::Eye.name().to_string() }
                    } else {
                        IconGlyph { name: TablerIcon::EyeOff.name().to_string() }
                    }
                }
                span {
                    {move || {
                        let (count, visible) = state.board_marks.get();
                        if visible { format!("Markup · {count}") } else { "Markup hidden".to_string() }
                    }}
                }
            }
        }
    };
    // The desktop's pixel scale reads the box the surface fills.
    BOUNDS.with(|b| b.borrow_mut()[pane.index()] = Some(view.bounds_signal()));
    // The theme's background, on every board.
    let _ = rinch::Effect::new(move || {
        let dark = store.dark_mode.get();
        untracked(|| set_dark_mode(dark));
    });
    view
}

/// The board tools in the one toolbar (docs/SPLIT_VIEW_CONTRACT.md "One
/// toolbar"), for the focused pane's board: select and pen, the pen's
/// color and width, undo and redo, delete, fit and 1:1. The editing tools are
/// dimmed on a board this device may only read.
pub(crate) fn render_board_toolbar(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let focused = move || store.focused_pane.get();
    let writable = move || {
        let pane = focused();
        store
            .pane(pane)
            .board
            .get()
            .is_some_and(|held| store.node_access_tracked(held.store_id, held.node_id).allows_write())
    };
    let tool_button = move |__scope: &mut RenderScope, tool: Tool, icon: TablerIcon, tip: &'static str| {
        rsx! {
            div {
                title: tip,
                class: {move || if store.board_tool.get() == tool { "pimble-board-tool pimble-board-tool--active" } else { "pimble-board-tool" }},
                onclick: move || store.board_tool.set(tool),
                IconGlyph { name: icon.name().to_string() }
            }
        }
    };
    let action = move |__scope: &mut RenderScope, icon: TablerIcon, tip: &'static str, run: fn(AppStore, PaneId)| {
        rsx! {
            div {
                title: tip,
                class: "pimble-board-tool",
                onclick: move || run(store, untracked(|| store.focused_pane.get())),
                IconGlyph { name: icon.name().to_string() }
            }
        }
    };
    let select = tool_button(__scope, Tool::Select, TablerIcon::Pointer, "Select (click, drag to move, Delete)");
    let pen = tool_button(__scope, Tool::Pen, TablerIcon::Pencil, "Pen");
    let swatches: Vec<NodeHandle> = COLORS
        .iter()
        .map(|color| {
            let color = *color;
            rsx! {
                div {
                    title: color,
                    class: {move || if store.board_color.get() == color { "pimble-board-swatch pimble-board-swatch--active" } else { "pimble-board-swatch" }},
                    style: format!("background: {color};"),
                    onclick: move || {
                        store.board_color.set(color.to_string());
                        store.board_tool.set(Tool::Pen);
                    },
                }
            }
        })
        .collect();
    let widths: Vec<NodeHandle> = WIDTHS
        .iter()
        .map(|width| {
            let width = *width;
            let dot = (width * 1.5 + 2.0).round();
            rsx! {
                div {
                    title: format!("Width {width}"),
                    class: {move || if store.board_width.get() == width { "pimble-board-tool pimble-board-tool--active" } else { "pimble-board-tool" }},
                    onclick: move || {
                        store.board_width.set(width);
                        store.board_tool.set(Tool::Pen);
                    },
                    div {
                        class: "pimble-board-width",
                        style: format!("width: {dot}px; height: {dot}px;"),
                    }
                }
            }
        })
        .collect();
    let undo_btn = action(__scope, TablerIcon::ArrowBackUp, "Undo (Ctrl+Z)", |store, pane| undo(store, pane, false));
    let redo_btn = action(__scope, TablerIcon::ArrowForwardUp, "Redo (Ctrl+Shift+Z)", |store, pane| undo(store, pane, true));
    let delete_btn = action(__scope, TablerIcon::Trash, "Delete the selection (Delete)", delete_selected);
    let fit_btn = action(__scope, TablerIcon::FocusCentered, "Fit", fit);
    let one_btn = action(__scope, TablerIcon::ZoomReset, "Actual size", actual_size);
    rsx! {
        div {
            class: "pimble-board-toolbar",
            div {
                class: "pimble-board-toolbar__group",
                style: {move || if writable() { "" } else { "opacity: 0.4; pointer-events: none;" }},
                {select}
                {pen}
                div { class: "pimble-board-toolbar__divider" }
                {swatches}
                div { class: "pimble-board-toolbar__divider" }
                {widths}
                div { class: "pimble-board-toolbar__divider" }
                {undo_btn}
                {redo_btn}
                {delete_btn}
            }
            div { class: "pimble-board-toolbar__divider" }
            {fit_btn}
            {one_btn}
        }
    }
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
        undo(store, P[0], false);
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
}
