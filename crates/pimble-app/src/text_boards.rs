//! Boards in text (docs/IMAGE_MARKUP_CONTRACT.md section 2): marked-up
//! pictures and board nodes embedded in a note's text, on the desktop and in
//! the browser from this one module.
//!
//! The text editor draws nothing of its own. A picture in text is rinch's
//! `image` atom; what its `<img>` asks the loader for is chosen from its
//! attributes ([`install_image_source`]):
//!
//! * a canvas in text (a picture, or one that began empty with `src`
//!   [`IN_TEXT_SRC`]) whose atom carries a `data-board` id asks for
//!   `<src>#canvas=<id>` (`#board=` before 2026-10-23, still read), and the
//!   loader answers with the board in the host node's `data["board:<id>"]`
//!   rendered (its visible extent, trimmed), or with the picture itself while
//!   nothing is drawn on it (docs/CANVAS_CONTRACT.md);
//! * a board node embedded in text has `src` `pimble-board:<store>/<node>`
//!   (`#frame=<id>` for one frame), and the loader answers with that board
//!   node rendered, live: while it is shown this window holds the board's
//!   document and its subscription.
//!
//! Rendering happens here on the UI thread's terms: the loader (on a thread
//! of rinch's own on the desktop) answers from [`RENDERED`] or asks for a
//! render; a render is debounced ([`RENDER_DEBOUNCE_MS`]) after every change
//! of what it shows, and `rinch::image::reload_image` hands the new picture
//! to every `<img>` naming it.
//!
//! What a host node's boards are read from is the host's document as this
//! window holds it ([`HOSTS`]): loaded when a pane opens the note, fed every
//! update the panes see (a peer's, a reconcile's, a board edit made here).
//!
//! The rest is the person's side: the floating controls over the picture
//! under the pointer (the markup toggle, and a menu with "Mark Up...",
//! "Convert to Node", "Open"), a click that opens an embed, a
//! double-click that marks up a picture, Insert > "Canvas", a board
//! node's link pasted, a tree row dropped on the text, and the board a
//! pasted marked-up picture brings with it.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use pimble_board::{render_area, render_extent, Align, Board, BoardSnapshot, Color, Geom, Kind, NewElement, Picture, Rect, Stamp, Style};
use pimble_core::{BlobUrl, BoardUrl, LinkResolution, NodeId, PimbleUrl, StoreId, MAX_LINK_HOPS};
use pimble_crdt::NodeDoc;
use rinch::image::ImageLoadResult;
use rinch::prelude::*;
use rinch_editor_core::{AttrValue, Attrs, Fragment, Node as EditorNode, SetNodeAttrStep};
use rinch_tabler_icons::TablerIcon;

/// The picture attribute naming its board in the host node's `data` root (`board:<id>`)
/// (contract section 2). A `data-*` attribute, which rinch keeps, writes on
/// the `<img>` and merges per attribute between collaborators; rinch knows
/// nothing of what it means.
pub(crate) const BOARD_ATTR: &str = "data-board";

use crate::appearance::IconGlyph;
use crate::panes::PaneId;
use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// The scheme of an embedded board's `src`, without the colon.
pub(crate) const BOARD_SCHEME: &str = "pimble-board";

/// What the source of a picture in text carries after its blob URL: the id
/// of its canvas in the host note's `data` (docs/CANVAS_CONTRACT.md,
/// decision 5; the same form Markdown export writes).
pub(crate) const CANVAS_FRAGMENT: &str = "#canvas=";

/// The fragment sources carried before canvases (2026-10-23); still read.
const BOARD_FRAGMENT: &str = "#board=";

/// The `src` of a canvas in text that began empty (Insert > "Canvas"):
/// there is no picture, and the atom's `data-board` names its canvas.
pub(crate) const IN_TEXT_SRC: &str = "pimble-board:in-text";

/// How long after the last change of a board its picture in text is drawn
/// again (contract section 2: a stroke being drawn appears as it is drawn,
/// without a render per point).
#[cfg_attr(test, allow(dead_code))]
const RENDER_DEBOUNCE_MS: u32 = 150;

/// How long the floating controls stay after the pointer leaves the picture,
/// so it can reach them.
const HIDE_GRACE_MS: u32 = 300;

/// Two releases this close on one picture are a double-click.
const DOUBLE_CLICK_MS: f64 = 450.0;

/// One board unit to one CSS pixel: a picture in text is its own size, and
/// the pane caps it.
const TEXT_SCALE: f32 = 1.0;

/// A rendered board in text is never wider or taller than this.
const MAX_TEXT_PX: f32 = 4096.0;

/// The placeholders an embed shows instead of someone else's pixels (contract
/// section 2, "It is a link").
const DELETED: &str = "This canvas was deleted.";
const NOT_ARRIVED: &str = "This canvas has not arrived yet.";
const NO_ACCESS: &str = "You cannot see this canvas.";
const EMPTY: &str = "This canvas is empty.";
/// An empty canvas in text, before anything is drawn on it.
const EMPTY_IN_TEXT: &str = "An empty canvas. Double-click to draw on it.";

/// "Convert to Node" refused.
const CONVERT_FAILED: &str = "The canvas could not be made into a node.";

/// Insert > "Canvas" with no note to put it in.
const NO_NOTE: &str = "Open a note to put a canvas in it.";

// ── Sources ─────────────────────────────────────────────────────────────────

/// What a picture source shown by this module names.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// A canvas in text: its picture (none for one that began empty) and its
    /// board in the host note's `data`.
    Marked { blob: Option<BlobUrl>, board: String },
    /// A board node embedded in text.
    Embed(BoardUrl),
}

fn parse_source(src: &str) -> Option<Source> {
    if let Some(url) = BoardUrl::parse(src) {
        return Some(Source::Embed(url));
    }
    let (blob, board) = src.split_once(CANVAS_FRAGMENT).or_else(|| src.split_once(BOARD_FRAGMENT))?;
    let blob = if blob == IN_TEXT_SRC { None } else { Some(BlobUrl::parse(blob)?) };
    let board = board.trim();
    if board.is_empty() || !board.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    Some(Source::Marked { blob, board: board.to_string() })
}

/// The source a canvas in text's `<img>` asks for.
pub(crate) fn marked_src(blob_src: &str, board: &str) -> String {
    format!("{blob_src}{CANVAS_FRAGMENT}{board}")
}

/// Whether the loader for `pimble-blob:` should hand `src` here.
pub(crate) fn is_marked_src(src: &str) -> bool {
    src.contains(CANVAS_FRAGMENT) || src.contains(BOARD_FRAGMENT)
}

/// Whether `src` is a canvas in text: a store's picture, or one that began empty.
pub(crate) fn is_canvas_src(src: &str) -> bool {
    src == IN_TEXT_SRC || BlobUrl::parse(src).is_some()
}

/// Choose each picture's source from its attributes, once per process: a
/// `board` on a blob picture asks for that board rendered over it. A
/// function of the attributes alone, as rinch asks.
pub(crate) fn install_image_source() {
    thread_local! {
        static INSTALLED: Cell<bool> = const { Cell::new(false) };
    }
    if INSTALLED.with(|i| i.replace(true)) {
        return;
    }
    // Outside any render, so it lasts as long as the app.
    untracked(|| {
        crate::rinch_editor::set_image_source(|attrs: &Attrs| {
            let board = attrs.get_str(BOARD_ATTR).filter(|b| !b.is_empty())?;
            let src = attrs.get_str("src")?;
            is_canvas_src(src).then(|| marked_src(src, board))
        })
    });
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| rinch::image::register_image_scheme(BOARD_SCHEME, load));
}

// ── The loader ──────────────────────────────────────────────────────────────

/// What the loader answers from, shared with the thread rinch calls it on.
#[derive(Default)]
struct Rendered {
    /// The latest rendering of each source, as PNG bytes.
    images: HashMap<String, Arc<Vec<u8>>>,
    /// Sources asked for and not answered yet.
    asked: HashSet<String>,
}

static RENDERED: Mutex<Option<Rendered>> = Mutex::new(None);

fn rendered<T>(f: impl FnOnce(&mut Rendered) -> T) -> T {
    let mut r = RENDERED.lock().unwrap_or_else(|p| p.into_inner());
    f(r.get_or_insert_with(Default::default))
}

/// rinch's loader for a board in text (both schemes come here): the latest
/// rendering if there is one, else "not yet" and a render asked for on the
/// UI thread. Never blocks.
pub(crate) fn load(src: &str) -> ImageLoadResult {
    let Some(source) = parse_source(src) else {
        return ImageLoadResult::Failed(format!("{src} is not a canvas's address"));
    };
    let (answer, first) = rendered(|r| match r.images.get(src) {
        Some(bytes) => (Some(bytes.to_vec()), false),
        None => (None, r.asked.insert(src.to_string())),
    });
    // Asked again (a reload, another element): it is shown, so the UI
    // thread makes sure it is held and current.
    let src_owned = src.to_string();
    if answer.is_none() || first {
        on_ui_thread(move || wanted(&src_owned));
    }
    match answer {
        Some(bytes) => ImageLoadResult::Loaded(bytes),
        None => match source {
            // The picture as it came in, while its board is drawn over it.
            Source::Marked { blob: Some(blob), .. } => match crate::pictures::held_bytes(&blob) {
                Some(bytes) => ImageLoadResult::Loaded(bytes.to_vec()),
                None => ImageLoadResult::Loaded(nothing_yet()),
            },
            Source::Marked { blob: None, .. } => ImageLoadResult::Loaded(nothing_yet()),
            // Not "failed": an `<img>` whose load failed is laid out as
            // nothing, and the line it is in is not wrapped again when the
            // picture comes (seen on the desktop); one pixel that shows
            // nothing is replaced like any picture.
            Source::Embed(_) => ImageLoadResult::Loaded(nothing_yet()),
        },
    }
}

/// A transparent pixel: what a board shows while it is being drawn.
fn nothing_yet() -> Vec<u8> {
    static PIXEL: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    PIXEL
        .get_or_init(|| pimble_board::RgbaImage { width: 1, height: 1, data: vec![0; 4] }.encode_png().unwrap_or_default())
        .clone()
}

/// Run `f` on the UI thread, a turn later (the loader runs on a thread of
/// rinch's own on the desktop, and while rinch is busy in the browser).
fn on_ui_thread(f: impl FnOnce() + Send + 'static) {
    #[cfg(test)]
    {
        let _ = f;
    }
    #[cfg(all(not(test), feature = "native"))]
    rinch::prelude::run_on_main_thread(move || {
        let _ = set_timeout(0, f);
    });
    #[cfg(all(not(test), not(feature = "native")))]
    {
        let _ = set_timeout(0, f);
    }
}

// ── Host documents ──────────────────────────────────────────────────────────

/// A note open in some pane, as this window holds its document: what its
/// pictures' boards are read from.
struct Host {
    doc: NodeDoc,
    panes: HashSet<PaneId>,
}

/// A board node embedded in text.
enum EmbedState {
    /// `resolveLink` asked.
    Resolving,
    /// Live at this canonical pair; its document once it is here.
    Live { store_id: StoreId, node_id: NodeId, doc: Option<NodeDoc> },
    /// One of the placeholder sentences.
    Gone(&'static str),
}

struct Embed {
    state: EmbedState,
    /// The sources showing it (with or without a frame).
    srcs: HashSet<String>,
}

thread_local! {
    static HOSTS: RefCell<HashMap<(StoreId, NodeId), Host>> = RefCell::new(HashMap::new());
    /// By the pair the embed names (before any `became`).
    static EMBEDS: RefCell<HashMap<(StoreId, NodeId), Embed>> = RefCell::new(HashMap::new());
    /// `resolveLink`s asked for embeds: the URL asked, and the embed it is for.
    static RESOLVING: RefCell<HashMap<String, (StoreId, NodeId)>> = RefCell::new(HashMap::new());
    /// Sources to render again, and the pending timer.
    static DIRTY: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static RENDER_TIMER: RefCell<Option<TimeoutHandle>> = const { RefCell::new(None) };
    /// The sweep of embeds no text shows any more.
    static SWEEP_TIMER: RefCell<Option<TimeoutHandle>> = const { RefCell::new(None) };
    /// Placeholder pictures, by sentence.
    static PLACEHOLDERS: RefCell<HashMap<&'static str, Arc<Vec<u8>>>> = RefCell::new(HashMap::new());
    /// "Convert to Node"s waiting for their node.
    static CONVERTS: RefCell<HashMap<u64, Convert>> = RefCell::new(HashMap::new());
    static NEXT_REQUEST: Cell<u64> = const { Cell::new(1 << 48) };
    /// The app, for callbacks rinch calls with nothing in hand.
    static APP: Cell<Option<AppStore>> = const { Cell::new(None) };
    /// Per pane: the pointer is over the floating controls, the hide timer,
    /// the controls' menu is open, and the last release on a picture.
    static OVER: RefCell<[bool; crate::panes::MAX_PANES]> = const { RefCell::new([false; crate::panes::MAX_PANES]) };
    static HIDE: RefCell<[Option<TimeoutHandle>; crate::panes::MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    static LAST_UP: RefCell<[(f64, usize); crate::panes::MAX_PANES]> = const { RefCell::new([(0.0, usize::MAX); crate::panes::MAX_PANES]) };
    /// Each pane's text area on screen.
    static TEXT_BOUNDS: RefCell<[Option<Signal<rinch::core::ElementBounds>>; crate::panes::MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
}

/// Where pane `pane`'s text is on screen (its scroller's box).
pub(crate) fn set_text_bounds(pane: PaneId, bounds: Signal<rinch::core::ElementBounds>) {
    TEXT_BOUNDS.with(|b| b.borrow_mut()[pane.index()] = Some(bounds));
}

pub(crate) fn text_bounds(pane: PaneId) -> Option<Signal<rinch::core::ElementBounds>> {
    TEXT_BOUNDS.with(|b| b.borrow()[pane.index()])
}

/// The widest a board in text is drawn: the widest pane's text, less its
/// margins (a board's natural size is one CSS pixel per unit, capped by the
/// pane, contract section 2).
fn text_width() -> f32 {
    let widest = PaneId::ALL
        .into_iter()
        .filter_map(text_bounds)
        .map(|b| untracked(|| b.get().width))
        .fold(0.0f32, f32::max);
    if widest > 0.0 {
        (widest - 64.0).max(160.0)
    } else {
        MAX_TEXT_PX
    }
}

fn app() -> Option<AppStore> {
    APP.with(Cell::get)
}

/// A pane opened note `node_id`: hold its document (shared by every pane of
/// this window holding it), from `bytes` (the content the editor joined).
pub(crate) fn host_opened(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    APP.with(|a| a.set(Some(store)));
    install_image_source();
    HOSTS.with(|hosts| {
        let mut hosts = hosts.borrow_mut();
        for host in hosts.values_mut() {
            host.panes.remove(&pane);
        }
        hosts.retain(|_, host| !host.panes.is_empty());
        let host = hosts.entry((store_id, node_id)).or_insert_with(|| Host {
            doc: NodeDoc::load(bytes).unwrap_or_else(|_| NodeDoc::new()),
            panes: HashSet::new(),
        });
        host.panes.insert(pane);
    });
    schedule_marked(store_id, node_id);
    schedule_sweep(store, 500);
}

/// Pane `pane` let go of its note.
pub(crate) fn host_closed(store: AppStore, pane: PaneId) {
    HOSTS.with(|hosts| {
        let mut hosts = hosts.borrow_mut();
        for host in hosts.values_mut() {
            host.panes.remove(&pane);
        }
        hosts.retain(|_, host| !host.panes.is_empty());
    });
    store.pane(pane).picture.set(None);
    schedule_sweep(store, 500);
}

/// The bytes of a note's document as this window holds it.
pub(crate) fn host_bytes(store_id: StoreId, node_id: NodeId) -> Option<Vec<u8>> {
    HOSTS.with(|hosts| hosts.borrow().get(&(store_id, node_id)).map(|h| h.doc.save()))
}

/// The host that holds board `board`, and the board read whole.
fn find_board(board: &str) -> Option<((StoreId, NodeId), pimble_board::RawBoard)> {
    HOSTS.with(|hosts| {
        hosts.borrow().iter().find_map(|(key, host)| Board::in_text(&host.doc, board).read().map(|raw| (*key, raw)))
    })
}

/// A board of `node`'s text as this window holds it.
fn board_snapshot(store_id: StoreId, node_id: NodeId, board: &str) -> Option<BoardSnapshot> {
    HOSTS.with(|hosts| {
        let hosts = hosts.borrow();
        let host = hosts.get(&(store_id, node_id))?;
        let b = Board::in_text(&host.doc, board);
        b.exists().then(|| b.snapshot())
    })
}

/// An update of node `node_id` (a peer's, a reconcile's, an edit made in
/// this window): its document here, and whatever shows it, follow.
/// `made_here`: the update was made in this window, perhaps on the host's
/// own document (the markup toggle, a copied board), where applying it again
/// changes nothing: what shows it is drawn again all the same.
fn applied(store_id: StoreId, node_id: NodeId, bytes: &[u8], made_here: bool) {
    let hosted = HOSTS.with(|hosts| {
        let mut hosts = hosts.borrow_mut();
        match hosts.get_mut(&(store_id, node_id)) {
            Some(host) => {
                match host.doc.apply_update(bytes) {
                    Ok(effect) => effect.data || made_here,
                    Err(e) => {
                        tracing::warn!("an update of note {node_id} did not apply to its boards: {e}");
                        false
                    }
                }
            }
            None => false,
        }
    });
    if hosted {
        schedule_marked(store_id, node_id);
        if let Some(store) = app() {
            refresh_hovers(store);
        }
    }
    let embeds = EMBEDS.with(|embeds| {
        let mut embeds = embeds.borrow_mut();
        let mut srcs = Vec::new();
        for embed in embeds.values_mut() {
            if let EmbedState::Live { store_id: s, node_id: n, doc } = &mut embed.state {
                if (*s, *n) == (store_id, node_id) {
                    let doc = doc.get_or_insert_with(NodeDoc::new);
                    if let Err(e) = doc.apply_update(bytes) {
                        tracing::warn!("an update of embedded board {node_id} did not apply: {e}");
                    }
                    srcs.extend(embed.srcs.iter().cloned());
                }
            }
        }
        srcs
    });
    if !embeds.is_empty() {
        schedule(embeds, false);
        if let Some(store) = app() {
            refresh_hovers(store);
        }
    }
}

/// A peer's update (`RemoteChanges`).
pub(crate) fn apply_remote(store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    applied(store_id, node_id, bytes, false);
}

/// The server's answer to a reconcile of the node (an editor's, a board
/// pane's, or an embed's own).
pub(crate) fn apply_reconcile(store_id: StoreId, node_id: NodeId, diff: &[u8]) {
    if !diff.is_empty() {
        applied(store_id, node_id, diff, false);
    }
}

/// An update this window made to a node's boards (a mark in the board view
/// over a pane, a board copied in, the markup toggle).
pub(crate) fn local_update(store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    applied(store_id, node_id, bytes, true);
}

// ── Rendering ───────────────────────────────────────────────────────────────

/// Every marked-up picture of the note that is shown, to be drawn again.
fn schedule_marked(store_id: StoreId, node_id: NodeId) {
    let srcs: Vec<String> = rendered(|r| r.asked.iter().chain(r.images.keys()).cloned().collect::<HashSet<_>>())
        .into_iter()
        .filter(|src| matches!(parse_source(src), Some(Source::Marked { .. })))
        .collect();
    // Which host holds a board is cheap to ask, but a board copied in names
    // a host that may not hold it yet: every marked source is looked at.
    let _ = (store_id, node_id);
    schedule(srcs, false);
}

/// Draw `srcs` again after the debounce (`now`: on the next turn).
fn schedule(srcs: impl IntoIterator<Item = String>, now: bool) {
    let mut any = false;
    DIRTY.with(|d| {
        let mut d = d.borrow_mut();
        for src in srcs {
            any |= d.insert(src);
        }
    });
    if !any && !now {
        return;
    }
    let pending = RENDER_TIMER.with(|t| t.borrow().is_some());
    if pending && !now {
        return;
    }
    if let Some(old) = RENDER_TIMER.with(|t| t.borrow_mut().take()) {
        clear_timeout(old);
    }
    #[cfg(test)]
    {
        flush();
    }
    #[cfg(not(test))]
    {
        let delay = if now { 0 } else { RENDER_DEBOUNCE_MS };
        let handle = set_timeout(delay, || {
            RENDER_TIMER.with(|t| t.borrow_mut().take());
            flush();
        });
        RENDER_TIMER.with(|t| *t.borrow_mut() = Some(handle));
    }
}

/// A shown source the UI thread has not drawn yet, or should look at again.
fn wanted(src: &str) {
    let Some(source) = parse_source(src) else { return };
    if let Source::Embed(url) = &source {
        hold_embed(url, src);
    }
    schedule([src.to_string()], true);
}

/// Draw every dirty source and hand rinch the new pictures.
fn flush() {
    let dirty: Vec<String> = DIRTY.with(|d| d.borrow_mut().drain().collect());
    for src in dirty {
        let Some(job) = prepare(&src) else { continue };
        match job {
            Job::Ready(bytes) => deliver(src, bytes),
            Job::Draw { snapshot, pictures, area } => draw(src, snapshot, pictures, area),
        }
    }
}

/// What drawing a source takes.
enum Job {
    /// Bytes at hand (a placeholder, a picture with no board yet).
    Ready(Arc<Vec<u8>>),
    /// A board to render: the whole visible extent, or one frame's rectangle.
    Draw { snapshot: BoardSnapshot, pictures: HashMap<String, Arc<Picture>>, area: Option<Rect> },
}

fn prepare(src: &str) -> Option<Job> {
    match parse_source(src)? {
        Source::Marked { blob, board } => {
            let snapshot = HOSTS.with(|hosts| {
                hosts.borrow().values().find_map(|host| {
                    let b = Board::in_text(&host.doc, &board);
                    b.exists().then(|| b.snapshot())
                })
            });
            let Some(blob) = blob else {
                // A canvas that began empty.
                return match snapshot {
                    None => Some(Job::Ready(placeholder(NOT_ARRIVED))),
                    Some(snapshot) if snapshot.render_rect().is_none() => Some(Job::Ready(placeholder(EMPTY_IN_TEXT))),
                    Some(snapshot) => {
                        let (pictures, _) = crate::boards::pictures_of(&snapshot);
                        Some(Job::Draw { snapshot, pictures, area: None })
                    }
                };
            };
            let Some(snapshot) = snapshot else {
                // No board here (yet): the picture as it came in.
                return held_picture(&blob).map(Job::Ready);
            };
            if shows_only_its_picture(&snapshot, &blob) {
                // Nothing drawn on it: the original picture, sharp at the
                // screen's scale (docs/CANVAS_CONTRACT.md, decision 2).
                return held_picture(&blob).map(Job::Ready);
            }
            let (pictures, all) = crate::boards::pictures_of(&snapshot);
            if !all {
                // Its picture is on its way: the plain picture meanwhile.
                return held_picture(&blob).map(Job::Ready);
            }
            Some(Job::Draw { snapshot, pictures, area: None })
        }
        Source::Embed(url) => {
            let key = (url.store, url.node);
            let state = EMBEDS.with(|embeds| {
                embeds.borrow().get(&key).map(|e| match &e.state {
                    EmbedState::Resolving => Err(None),
                    EmbedState::Gone(sentence) => Err(Some(*sentence)),
                    EmbedState::Live { doc: None, .. } => Err(None),
                    EmbedState::Live { doc: Some(doc), .. } => {
                        let board = Board::of_node(doc);
                        if board.exists() {
                            Ok(board.snapshot())
                        } else if doc.fields().is_ok_and(|f| f.deleted_at.is_some()) {
                            Err(Some(DELETED))
                        } else {
                            Err(Some(NOT_ARRIVED))
                        }
                    }
                })
            })?;
            let snapshot = match state {
                Ok(snapshot) => snapshot,
                Err(Some(sentence)) => return Some(Job::Ready(placeholder(sentence))),
                Err(None) => return None,
            };
            let area = url.frame.as_deref().and_then(|frame| {
                let e = snapshot.get(frame).filter(|e| e.raw.kind == Kind::Frame)?;
                let r = e.pivot_bounds;
                Some(Rect::new(r.x + e.world.tx, r.y + e.world.ty, r.w * e.world.sx, r.h * e.world.sy))
            });
            if snapshot.render_rect().is_none() && area.is_none() {
                return Some(Job::Ready(placeholder(EMPTY)));
            }
            let (pictures, _) = crate::boards::pictures_of(&snapshot);
            Some(Job::Draw { snapshot, pictures, area })
        }
    }
}

/// Whether a canvas in text shows nothing but the picture it began with, as
/// it began: no marks, no page, and the picture alone, at one unit per pixel
/// where it was put, uncropped.
fn shows_only_its_picture(snapshot: &BoardSnapshot, blob: &BlobUrl) -> bool {
    let [only] = snapshot.elements.as_slice() else { return false };
    let Geom::Picture(p) = &only.raw.geom else { return false };
    snapshot.page.is_none()
        && p.src == blob.to_string()
        && p.crop.is_none()
        && p.rect == Rect::new(0.0, 0.0, p.px_w as f32, p.px_h as f32)
        && only.raw.transform == pimble_board::Transform::IDENTITY
}

/// A picture's own bytes, when this window has them (asked for when not).
fn held_picture(blob: &BlobUrl) -> Option<Arc<Vec<u8>>> {
    let held = crate::pictures::held_bytes(blob);
    if held.is_none() {
        if let Some(store) = app() {
            store.send(BackendCommand::GetBlob { url: *blob });
        }
    }
    held
}

/// Render a board, off the UI thread on the desktop.
fn draw(src: String, snapshot: BoardSnapshot, pictures: HashMap<String, Arc<Picture>>, area: Option<Rect>) {
    let width = text_width();
    let work = move || -> Option<Vec<u8>> {
        let fit = |rect: &Rect| TEXT_SCALE.min(MAX_TEXT_PX / rect.w.max(rect.h).max(1.0)).min(width / rect.w.max(1.0));
        let rendered = match area {
            Some(rect) => {
                let scale = fit(&rect);
                let fill = snapshot.page.as_ref().and_then(|p| p.fill).unwrap_or(Color::WHITE);
                render_area(&snapshot, &pictures, rect, scale, Some(fill), None, false)
            }
            None => {
                let rect = snapshot.render_rect()?;
                render_extent(&snapshot, &pictures, fit(&rect))
            }
        }?;
        rendered.image.encode_png().ok()
    };
    #[cfg(all(feature = "native", not(test)))]
    std::thread::spawn(move || {
        let bytes = work();
        run_on_main_thread(move || {
            if let Some(bytes) = bytes {
                deliver(src, Arc::new(bytes));
            }
        });
    });
    #[cfg(any(not(feature = "native"), test))]
    {
        if let Some(bytes) = work() {
            deliver(src, Arc::new(bytes));
        }
    }
}

/// A new picture for `src`: kept for the loader, and every `<img>` naming it
/// asks again. Nothing happens when it is the picture already shown.
fn deliver(src: String, bytes: Arc<Vec<u8>>) {
    let changed = rendered(|r| {
        r.asked.remove(&src);
        match r.images.get(&src) {
            Some(old) if **old == *bytes => false,
            _ => {
                r.images.insert(src.clone(), bytes);
                true
            }
        }
    });
    if changed {
        rinch::image::reload_image(&src);
    }
}

/// A picture of one of the placeholder sentences.
fn placeholder(sentence: &'static str) -> Arc<Vec<u8>> {
    if let Some(bytes) = PLACEHOLDERS.with(|p| p.borrow().get(sentence).cloned()) {
        return bytes;
    }
    let board = Board::standalone();
    let width = (sentence.chars().count() as f32 * 8.5 + 48.0).max(240.0);
    // An empty canvas in text is a space to draw in, not a notice.
    let (width, height) = if sentence == EMPTY_IN_TEXT { (width.max(480.0), 200.0) } else { (width, 52.0) };
    let style = Style {
        stroke: Some(Color::rgb(0x9e, 0x9e, 0x9e)),
        fill: Some(Color::rgb(0xf1, 0xf3, 0xf5)),
        width: Some(1.0),
        font_size: Some(15.0),
        align: Some(Align::Center),
        ..Style::default()
    };
    let _ = board.edit(Stamp::new(None, "1970-01-01T00:00:00.000Z"), |t| {
        t.create()?;
        t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, width, height))).with_style(style).with_text(sentence))
    });
    let bytes = render_extent(&board.snapshot(), &pimble_board::NoPictures, 1.0)
        .and_then(|r| r.image.encode_png().ok())
        .unwrap_or_default();
    let bytes = Arc::new(bytes);
    PLACEHOLDERS.with(|p| p.borrow_mut().insert(sentence, bytes.clone()));
    bytes
}

/// A picture arrived for the boards: whatever shows one is drawn again.
pub(crate) fn picture_arrived() {
    let srcs: Vec<String> = rendered(|r| r.asked.iter().chain(r.images.keys()).cloned().collect::<HashSet<_>>()).into_iter().collect();
    schedule(srcs, false);
}

// ── Embeds ──────────────────────────────────────────────────────────────────

/// Hold the board node `url` names while `src` shows it: resolve it (the
/// server judges the read and follows `became`), then subscribe and fetch.
fn hold_embed(url: &BoardUrl, src: &str) {
    let key = (url.store, url.node);
    let fresh = EMBEDS.with(|embeds| {
        let mut embeds = embeds.borrow_mut();
        match embeds.get_mut(&key) {
            Some(embed) => {
                embed.srcs.insert(src.to_string());
                false
            }
            None => {
                embeds.insert(key, Embed { state: EmbedState::Resolving, srcs: HashSet::from([src.to_string()]) });
                true
            }
        }
    });
    if fresh {
        resolve(key, url.link(), 0);
    }
}

fn resolve(key: (StoreId, NodeId), url: PimbleUrl, hops: usize) {
    let Some(store) = app() else { return };
    RESOLVING.with(|r| r.borrow_mut().insert(url.to_string(), key));
    store.send(BackendCommand::ResolveLink { url, hops });
}

/// A `resolveLink` answer: `true` when it was an embed's (and taken here).
pub(crate) fn link_resolved(store: AppStore, url: &PimbleUrl, hops: usize, resolution: &LinkResolution) -> bool {
    let Some(key) = RESOLVING.with(|r| r.borrow_mut().remove(&url.to_string())) else { return false };
    let state = match resolution {
        LinkResolution::Live { store_id, node_id } => EmbedState::Live { store_id: *store_id, node_id: *node_id, doc: None },
        LinkResolution::Deleted { .. } => EmbedState::Gone(DELETED),
        LinkResolution::NoAccess => EmbedState::Gone(NO_ACCESS),
        LinkResolution::StoreNotHere { store_id, node_id }
            if *store_id != url.store && hops < MAX_LINK_HOPS && store.get_store_signal(*store_id).is_some() =>
        {
            resolve(key, PimbleUrl::node(*store_id, *node_id), hops + 1);
            return true;
        }
        LinkResolution::StoreNotHere { .. } | LinkResolution::Missing => EmbedState::Gone(NOT_ARRIVED),
    };
    let live = match &state {
        EmbedState::Live { store_id, node_id, .. } => Some((*store_id, *node_id)),
        _ => None,
    };
    // Whether the node is held already (a pane, another embed) before this
    // embed counts as holding it.
    let held = live.is_some_and(|(s, n)| store.is_editing(s, n));
    let srcs = EMBEDS.with(|embeds| {
        let mut embeds = embeds.borrow_mut();
        let embed = embeds.get_mut(&key)?;
        embed.state = state;
        Some(embed.srcs.iter().cloned().collect::<Vec<_>>())
    });
    let Some(srcs) = srcs else {
        return true;
    };
    if let Some((store_id, node_id)) = live {
        if !held {
            store.send(BackendCommand::SubscribeNodeChanges { store_id, node_id });
        }
        store.send(BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector: NodeDoc::new().state_vector() });
    }
    schedule(srcs, true);
    true
}

/// Whether an embed in this window holds the node's subscription.
pub(crate) fn embed_holds(store_id: StoreId, node_id: NodeId) -> bool {
    EMBEDS.with(|embeds| {
        embeds.borrow().values().any(|e| matches!(&e.state, EmbedState::Live { store_id: s, node_id: n, .. } if (*s, *n) == (store_id, node_id)))
    })
}

/// The connection came back: every embed's subscription died with it.
pub(crate) fn reconnected(store: AppStore) {
    let live: Vec<(StoreId, NodeId, Vec<u8>)> = EMBEDS.with(|embeds| {
        embeds
            .borrow()
            .values()
            .filter_map(|e| match &e.state {
                EmbedState::Live { store_id, node_id, doc } => {
                    Some((*store_id, *node_id, doc.as_ref().map(|d| d.state_vector()).unwrap_or_else(|| NodeDoc::new().state_vector())))
                }
                _ => None,
            })
            .collect()
    });
    let edited = store.edited_nodes();
    let mut done = HashSet::new();
    for (store_id, node_id, state_vector) in live {
        if !done.insert((store_id, node_id)) {
            continue;
        }
        if !edited.iter().any(|e| (e.store_id, e.node_id) == (store_id, node_id)) {
            store.send(BackendCommand::SubscribeNodeChanges { store_id, node_id });
        }
        store.send(BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector });
    }
}

/// Every board source the open notes show, read from their editors.
fn shown_sources(store: AppStore) -> HashSet<String> {
    let mut out = HashSet::new();
    for pane in PaneId::ALL {
        if untracked(|| store.pane(pane).active_edit.get()).is_none() {
            continue;
        }
        for (_, attrs) in images(&crate::editor::editor(pane).doc()) {
            let Some(src) = attrs.get_str("src") else { continue };
            if BoardUrl::parse(src).is_some() {
                out.insert(src.to_string());
            }
        }
    }
    out
}

fn schedule_sweep(store: AppStore, ms: u32) {
    if let Some(old) = SWEEP_TIMER.with(|t| t.borrow_mut().take()) {
        clear_timeout(old);
    }
    // Tests sweep when they mean to: a sweep reads the editors, and this is
    // called from inside an editor's edit.
    #[cfg(test)]
    {
        let _ = (store, ms);
    }
    #[cfg(not(test))]
    {
        let handle = set_timeout(ms, move || {
            SWEEP_TIMER.with(|t| t.borrow_mut().take());
            sweep(store);
        });
        SWEEP_TIMER.with(|t| *t.borrow_mut() = Some(handle));
    }
}

/// A note's text changed: an embed may have come or gone.
pub(crate) fn text_changed(store: AppStore) {
    schedule_sweep(store, 1500);
}

/// Let go of every embed no open text shows (its subscription with it, when
/// nothing else holds the node), and hold every one that came back (an undo
/// brings an embed back that rinch remembers and never asks for again).
fn sweep(store: AppStore) {
    let shown = shown_sources(store);
    let gone: Vec<((StoreId, NodeId), Option<(StoreId, NodeId)>, Vec<String>)> = EMBEDS.with(|embeds| {
        let mut embeds = embeds.borrow_mut();
        let keys: Vec<(StoreId, NodeId)> = embeds.iter().filter(|(_, e)| !e.srcs.iter().any(|s| shown.contains(s))).map(|(k, _)| *k).collect();
        keys.into_iter()
            .filter_map(|key| {
                let embed = embeds.remove(&key)?;
                let live = match embed.state {
                    EmbedState::Live { store_id, node_id, .. } => Some((store_id, node_id)),
                    _ => None,
                };
                Some((key, live, embed.srcs.into_iter().collect()))
            })
            .collect()
    });
    for (_, live, srcs) in gone {
        rendered(|r| {
            for src in &srcs {
                r.images.remove(src);
                r.asked.remove(src);
            }
        });
        if let Some((store_id, node_id)) = live {
            if !store.is_editing(store_id, node_id) {
                store.send(BackendCommand::UnsubscribeNodeChanges { store_id, node_id });
            }
        }
    }
    for src in shown {
        if let Some(Source::Embed(url)) = parse_source(&src) {
            let held = EMBEDS.with(|e| e.borrow().get(&(url.store, url.node)).is_some_and(|e| e.srcs.contains(&src)));
            if !held {
                wanted(&src);
            }
        }
    }
}

// ── Pictures in the editor's model ──────────────────────────────────────────

/// Every image in `doc`, with the position before it.
fn images(doc: &EditorNode) -> Vec<(usize, Attrs)> {
    let mut out = Vec::new();
    doc.nodes_between(0, doc.content_size(), &mut |node, pos, _| {
        if node.type_name() == "image" {
            out.push((pos, node.attrs().clone()));
            return false;
        }
        true
    });
    out
}

/// The image in `doc` that is the picture `src` (with board `board`, or none)
/// nearest to `hint`: a picture is found again by what it is, wherever typing
/// has moved it.
fn find_image(doc: &EditorNode, src: &str, board: Option<&str>, hint: usize) -> Option<usize> {
    images(doc)
        .into_iter()
        .filter(|(_, attrs)| attrs.get_str("src") == Some(src) && attrs.get_str(BOARD_ATTR).filter(|b| !b.is_empty()) == board)
        .min_by_key(|(pos, _)| pos.abs_diff(hint))
        .map(|(pos, _)| pos)
}

/// Write `board` on the picture `src` in pane `pane`'s text: the atom's
/// attribute, through the editor, so it is one more edit of the text.
pub(crate) fn write_board_attr(pane: PaneId, src: &str, hint: usize, board: &str) -> bool {
    let (src, board) = (src.to_string(), board.to_string());
    crate::editor::editor(pane).update(move |state| {
        let pos = find_image(&state.doc, &src, None, hint)?;
        let mut tr = state.tr();
        tr.step(Box::new(SetNodeAttrStep::new(pos, BOARD_ATTR, AttrValue::from(board.as_str())))).ok()?;
        Some(tr)
    })
}

/// Set the width the picture `src` (with markup board `board`) shows at in
/// pane `pane`'s text: the atom's `width`, through the editor, so it is one
/// more co-edited edit of the text and every device shows the same size.
fn write_width(pane: PaneId, src: &str, board: Option<&str>, hint: usize, width: i64) -> bool {
    let (src, board) = (src.to_string(), board.map(str::to_string));
    crate::editor::editor(pane).update(move |state| {
        let pos = find_image(&state.doc, &src, board.as_deref(), hint)?;
        let mut tr = state.tr();
        tr.step(Box::new(SetNodeAttrStep::new(pos, "width", AttrValue::Int(width)))).ok()?;
        Some(tr)
    })
}

/// The narrowest a picture can be made by dragging a grip.
const MIN_PICTURE_PX: f32 = 32.0;

/// An image atom showing board node `url`, titled `title`.
pub(crate) fn embed_node(schema: &rinch_editor_core::Schema, url: &BoardUrl, title: &str) -> Option<EditorNode> {
    let attrs = Attrs::new().with("src", AttrValue::from(url.to_string())).with("alt", AttrValue::from(title));
    schema.create_node("image", attrs, Fragment::empty()).ok()
}

/// Put board node `url` into pane `pane`'s text over `at` (a range; the
/// caret when `None`), as a picture.
pub(crate) fn insert_embed(store: AppStore, pane: PaneId, url: BoardUrl, at: Option<(usize, usize)>) -> bool {
    let Some(active) = untracked(|| store.pane(pane).active_edit.get()) else {
        crate::events::show_notice(store, NO_NOTE.to_string());
        return false;
    };
    if !store.node_access(active.store_id, active.node_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return false;
    }
    let title = board_title(store, url.store, url.node);
    crate::editor::editor(pane).update(move |state| {
        let node = embed_node(state.schema(), &url, &title)?;
        let mut tr = state.tr();
        if let Some((from, to)) = at {
            tr.set_selection(rinch_editor_core::Selection::text(rinch_editor_core::Pos(from), rinch_editor_core::Pos(to)));
        }
        tr.replace_selection_with(node).ok()?;
        Some(tr)
    })
}

/// A board node's title as this window knows it.
fn board_title(store: AppStore, store_id: StoreId, node_id: NodeId) -> String {
    store
        .get_node_signal(store_id, node_id)
        .map(|sig| untracked(|| sig.with(|n| n.metadata.title.clone())))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Canvas".to_string())
}

/// Whether node `node_id` is a board node, as this window knows it.
pub(crate) fn is_board_node(store: AppStore, store_id: StoreId, node_id: NodeId) -> bool {
    store
        .get_node_signal(store_id, node_id)
        .is_some_and(|sig| untracked(|| sig.with(|n| n.node_type == pimble_core::node_types::BOARD)))
}

/// A tree row dropped on pane `pane`'s text: a board node goes in as an
/// embed, at the caret. Anything else is not this drop's business.
pub(crate) fn drop_tree_row(store: AppStore, pane: PaneId, value: &str) -> bool {
    let Some((store_id, Some(node_id))) = crate::state::parse_tree_value(value) else { return false };
    if !is_board_node(store, store_id, node_id) {
        return false;
    }
    insert_embed(store, pane, BoardUrl::new(store_id, node_id), None)
}

/// Insert > "Canvas": an empty canvas at the caret (docs/CANVAS_CONTRACT.md,
/// decision 4). An existing canvas node goes into text by dragging its row
/// onto the text, or by pasting its link.
pub fn insert_board(store: AppStore) {
    let pane = store.focused();
    let Some(active) = untracked(|| store.pane(pane).active_edit.get()) else {
        crate::events::show_notice(store, NO_NOTE.to_string());
        return;
    };
    if !store.node_access(active.store_id, active.node_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    if !insert_canvas(store, pane, active.store_id, active.node_id, None, "", None) {
        crate::events::show_notice(store, "A canvas cannot go where the caret is.".to_string());
    }
    crate::editor::editor(pane).focus();
}

/// Put a new canvas into pane `pane`'s text, the note `(store_id, node_id)`:
/// empty, or starting from `picture` (its URL and pixel size) with `alt`. It
/// goes over `at` (the caret when `None`), and the person's own selection is
/// kept where it is in the text.
///
/// Two updates, the canvas first (docs/CANVAS_CONTRACT.md, decision 6): the
/// canvas in the note's `data`, then the atom naming it as one edit of the
/// text, so a peer never sees a picture naming a canvas it has not got.
/// `false` when the text took nothing; the canvas then stays in `data`
/// unreferenced, as a deleted picture's does.
pub(crate) fn insert_canvas(
    store: AppStore,
    pane: PaneId,
    store_id: StoreId,
    node_id: NodeId,
    picture: Option<(&BlobUrl, u32, u32)>,
    alt: &str,
    at: Option<rinch_editor_core::Selection>,
) -> bool {
    let board_id = pimble_board::doc::new_id();
    let author = crate::shares::author_in(store, store_id, node_id);
    let update = HOSTS.with(|hosts| {
        let hosts = hosts.borrow();
        let host = hosts.get(&(store_id, node_id))?;
        crate::boards::build_board(&Board::in_text(&host.doc, &board_id), author.as_ref(), picture)
    });
    let Some(update) = update else { return false };
    crate::boards::send_node_update(store, store_id, node_id, update);
    let src = picture.map(|(url, _, _)| url.to_string()).unwrap_or_else(|| IN_TEXT_SRC.to_string());
    let alt = alt.to_string();
    crate::editor::editor(pane).update(move |state| {
        let attrs = Attrs::new()
            .with("src", AttrValue::from(src.as_str()))
            .with("alt", AttrValue::from(alt.as_str()))
            .with(BOARD_ATTR, AttrValue::from(board_id.as_str()));
        let node = state.schema().create_node("image", attrs, Fragment::empty()).ok()?;
        let mut tr = state.tr();
        let moved = at.as_ref().is_some_and(|at| *at != state.selection);
        if let Some(at) = at.clone() {
            tr.set_selection(at);
        }
        tr.replace_selection_with(node).ok()?;
        if moved {
            let live = state.selection.map(tr.doc(), tr.mapping());
            tr.set_selection(live);
        }
        Some(tr)
    })
}

// ── Pasting pictures ─────────────────────────────────────────────────────────

/// What a pasted picture's canvas is made from.
enum PastedCanvas {
    /// A copy of the canvas (by its old id) the picture named.
    Copy(String),
    /// A new canvas from the picture alone (its URL and pixel size): a
    /// picture copied from text written before every picture was a canvas.
    Fresh(BlobUrl, u32, u32),
    /// A new empty canvas: one that began empty, from a canvas this window
    /// does not hold.
    Empty,
}

/// The pasted pictures' canvases (docs/IMAGE_MARKUP_CONTRACT.md section 2, "Copy
/// and paste", and docs/CANVAS_CONTRACT.md section 3): every image in `slice`
/// with a `data-board` gets a fresh id, and the canvas it names is copied under
/// that id into pane `pane`'s note a turn later; a store's picture with none
/// gets a new canvas of its own. A canvas this window does not hold (copied in
/// another window) cannot come along, nor can a picture whose bytes it has
/// not got be measured: the picture is pasted as it came in, and becomes a
/// canvas at its first mark. Answers the slice to paste, or `None` when no
/// pasted picture changed.
pub(crate) fn rebind_pasted_boards(schema: &rinch_editor_core::Schema, slice: &rinch_editor_core::Slice, pane: PaneId) -> Option<rinch_editor_core::Slice> {
    fn canvas_for(attrs: &Attrs) -> Option<PastedCanvas> {
        if let Some(old) = attrs.get_str(BOARD_ATTR).filter(|b| !b.is_empty() && find_board(b).is_some()) {
            return Some(PastedCanvas::Copy(old.to_string()));
        }
        let src = attrs.get_str("src")?;
        if src == IN_TEXT_SRC {
            return Some(PastedCanvas::Empty);
        }
        let blob = BlobUrl::parse(src)?;
        let picture = crate::pictures::held_bytes(&blob).and_then(|bytes| Picture::decode(&bytes).ok())?;
        Some(PastedCanvas::Fresh(blob, picture.width(), picture.height()))
    }
    fn concerns(node: &EditorNode) -> bool {
        node.type_name() == "image" && (node.attrs().get_str(BOARD_ATTR).is_some_and(|b| !b.is_empty()) || node.attrs().get_str("src").is_some_and(is_canvas_src))
    }
    fn walk(node: &EditorNode, schema: &rinch_editor_core::Schema, jobs: &mut Vec<(PastedCanvas, String)>) -> EditorNode {
        if node.type_name() == "image" {
            if !concerns(node) {
                return node.clone();
            }
            let mut attrs = node.attrs().without(BOARD_ATTR);
            if let Some(canvas) = canvas_for(node.attrs()) {
                let fresh = pimble_board::doc::new_id();
                jobs.push((canvas, fresh.clone()));
                attrs = attrs.with(BOARD_ATTR, AttrValue::from(fresh.as_str()));
            }
            return schema
                .create_node("image", attrs, Fragment::empty())
                .map(|n| n.with_marks(node.marks().to_vec()))
                .unwrap_or_else(|_| node.clone());
        }
        if node.is_text() || node.child_count() == 0 {
            return node.clone();
        }
        let children: Vec<EditorNode> = node.content().iter().map(|child| walk(child, schema, jobs)).collect();
        node.copy_with_content(Fragment::from_children(children))
    }
    let mut any = false;
    slice.content.iter().for_each(|n| {
        n.nodes_between(0, n.content_size(), &mut |c, _, _| {
            any |= concerns(c);
            true
        });
        any |= concerns(n);
    });
    if !any {
        return None;
    }
    let mut jobs = Vec::new();
    let top: Vec<EditorNode> = slice.content.iter().map(|n| walk(n, schema, &mut jobs)).collect();
    if !jobs.is_empty() {
        // The paste is mid-dispatch: the canvases are made a turn later
        // (they touch nothing of the editor; under `cargo test` no timer
        // runs, so at once). The text's update follows the paste, so a peer
        // can see a picture a moment before its canvas, which is drawn as the
        // plain picture meanwhile.
        #[cfg(not(test))]
        crate::links::later(move |store| copy_boards(store, pane, jobs));
        #[cfg(test)]
        if let Some(store) = crate::links::with_app_store(|store| store) {
            copy_boards(store, pane, jobs);
        }
    }
    Some(rinch_editor_core::Slice::new(Fragment::from_children(top), slice.open_start, slice.open_end))
}

/// Make each pasted picture's canvas in pane `pane`'s note under its new id.
fn copy_boards(store: AppStore, pane: PaneId, jobs: Vec<(PastedCanvas, String)>) {
    let Some(active) = untracked(|| store.pane(pane).active_edit.get()) else { return };
    let author = crate::shares::author_in(store, active.store_id, active.node_id);
    for (canvas, new) in jobs {
        let raw = match &canvas {
            PastedCanvas::Copy(old) => match find_board(old) {
                Some((_, raw)) => Some(raw),
                None => continue,
            },
            PastedCanvas::Fresh(..) | PastedCanvas::Empty => None,
        };
        let Some(update) = HOSTS.with(|hosts| {
            let hosts = hosts.borrow();
            let host = hosts.get(&(active.store_id, active.node_id))?;
            let board = Board::in_text(&host.doc, &new);
            match (&canvas, raw) {
                (_, Some(raw)) => {
                    let now = crate::boards::now_stamp();
                    board.edit(Stamp::new(author.as_ref(), &now), |t| t.copy_from(&raw)).update
                }
                (PastedCanvas::Fresh(blob, w, h), None) => crate::boards::build_board(&board, author.as_ref(), Some((blob, *w, *h))),
                (PastedCanvas::Empty, None) => crate::boards::build_board(&board, author.as_ref(), None),
                (PastedCanvas::Copy(_), None) => None,
            }
        }) else {
            continue;
        };
        crate::boards::send_node_update(store, active.store_id, active.node_id, update);
    }
}

// ── The floating controls ───────────────────────────────────────────────────

/// The picture under the pointer in a pane's text, and what its controls
/// show.
#[derive(Debug, Clone, PartialEq)]
pub struct PictureHover {
    /// The position before the image.
    pub pos: usize,
    pub src: String,
    pub alt: String,
    /// The board of its markup, for a picture.
    pub board: Option<String>,
    /// The board node it shows, for an embed.
    pub embed: Option<BoardUrl>,
    /// Its box (left, top, width, height), in the frame a fixed overlay uses.
    pub rect: (f32, f32, f32, f32),
    /// How many marks its board has and whether they are shown, when it has
    /// a board.
    pub marks: Option<(usize, bool)>,
}

impl PictureHover {
    /// Whether it is a canvas in text (a store's picture, or a canvas that
    /// began empty), not an embed and not a web picture.
    pub fn is_picture(&self) -> bool {
        self.embed.is_none() && (BlobUrl::parse(&self.src).is_some() || (self.src == IN_TEXT_SRC && self.board.is_some()))
    }
}

/// The marks of what `hover` shows, read now.
fn marks_of(store: AppStore, pane: PaneId, hover: &PictureHover) -> Option<(usize, bool)> {
    let count = |s: &BoardSnapshot| (s.markup_count(), s.markup_visible);
    if let Some(url) = &hover.embed {
        return EMBEDS.with(|embeds| match &embeds.borrow().get(&(url.store, url.node))?.state {
            EmbedState::Live { doc: Some(doc), .. } => {
                let board = Board::of_node(doc);
                board.exists().then(|| count(&board.snapshot()))
            }
            _ => None,
        });
    }
    let board = hover.board.as_deref()?;
    let active = untracked(|| store.pane(pane).active_edit.get())?;
    board_snapshot(active.store_id, active.node_id, board).map(|s| count(&s))
}

/// The editor's image hover (rinch's `on_image_hover`): the controls follow
/// the picture under the pointer, and stay a moment after it leaves.
pub(crate) fn hovered(store: AppStore, pane: PaneId, hover: Option<&crate::rinch_editor::ImageHover>) {
    let Some(h) = hover else {
        hide_later(store, pane);
        return;
    };
    let src = h.attrs.get_str("src").unwrap_or_default().to_string();
    let embed = BoardUrl::parse(&src);
    if embed.is_none() && !is_canvas_src(&src) {
        hide_later(store, pane);
        return;
    }
    cancel_hide(pane);
    let mut picture = PictureHover {
        pos: h.pos.0,
        alt: h.attrs.get_str("alt").unwrap_or_default().to_string(),
        board: h.attrs.get_str(BOARD_ATTR).filter(|b| !b.is_empty()).map(str::to_string),
        embed,
        src,
        rect: (h.rect.x, h.rect.y, h.rect.width, h.rect.height),
        marks: None,
    };
    picture.marks = marks_of(store, pane, &picture);
    store.pane(pane).picture.set_if_changed(Some(picture));
}

/// The marks of every shown control, read again (a board changed).
fn refresh_hovers(store: AppStore) {
    for pane in PaneId::ALL {
        let Some(mut hover) = untracked(|| store.pane(pane).picture.get()) else { continue };
        hover.marks = marks_of(store, pane, &hover);
        store.pane(pane).picture.set_if_changed(Some(hover));
    }
}

fn cancel_hide(pane: PaneId) {
    if let Some(old) = HIDE.with(|h| h.borrow_mut()[pane.index()].take()) {
        clear_timeout(old);
    }
}

fn hide_later(store: AppStore, pane: PaneId) {
    cancel_hide(pane);
    let handle = set_timeout(HIDE_GRACE_MS, move || {
        HIDE.with(|h| h.borrow_mut()[pane.index()] = None);
        if !OVER.with(|o| o.borrow()[pane.index()]) {
            store.pane(pane).picture.set(None);
        }
    });
    HIDE.with(|h| h.borrow_mut()[pane.index()] = Some(handle));
}

/// The pane's text scrolled: the picture moved from under the controls
/// (rinch reports nothing on a scroll, and on the desktop never clears its
/// hover then: joeleaver/rinch#1506).
pub(crate) fn scrolled(store: AppStore, pane: PaneId) {
    cancel_hide(pane);
    OVER.with(|o| o.borrow_mut()[pane.index()] = false);
    if untracked(|| store.pane(pane).picture.with(Option::is_some)) {
        store.pane(pane).picture.set(None);
    }
}

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

/// A release in pane `pane`: Ctrl/Cmd on an embed opens its board, as a link
/// is followed; a plain click leaves the embed selected, like any picture
/// (Joe, 2026-10-20: clicking an embedded board should not navigate). The
/// second of a double-click opens an embed's board, or marks a picture up.
/// Releases over the controls are theirs.
pub(crate) fn pane_mouseup(store: AppStore, pane: PaneId) {
    if OVER.with(|o| o.borrow()[pane.index()]) {
        return;
    }
    let Some(hover) = untracked(|| store.pane(pane).picture.get()) else { return };
    let ctx = rinch::core::get_click_context();
    if ctx.button != rinch::core::MouseButton::Left {
        return;
    }
    let (x, y, w, h) = hover.rect;
    if !(ctx.mouse_x >= x && ctx.mouse_x <= x + w && ctx.mouse_y >= y && ctx.mouse_y <= y + h) {
        return;
    }
    // A drag that selected text and ended on the picture is not a click.
    let selection = crate::editor::editor(pane).selection();
    if selection.from() != selection.to() && (selection.from().0, selection.to().0) != (hover.pos, hover.pos + 1) {
        return;
    }
    if let (Some(url), true) = (&hover.embed, ctx.modifiers.ctrl || ctx.modifiers.meta) {
        open_embed(store, pane, url, false);
        return;
    }
    let now = now_ms();
    let double = LAST_UP.with(|l| {
        let mut l = l.borrow_mut();
        let (at, pos) = l[pane.index()];
        let double = pos == hover.pos && now - at < DOUBLE_CLICK_MS;
        l[pane.index()] = if double { (0.0, usize::MAX) } else { (now, hover.pos) };
        double
    });
    if double {
        match &hover.embed {
            Some(url) => open_embed(store, pane, url, false),
            None if hover.is_picture() => mark_up(store, pane, &hover),
            None => {}
        }
    }
}

/// Open the board node an embed shows: in pane `pane`, or beside it.
fn open_embed(store: AppStore, pane: PaneId, url: &BoardUrl, split: bool) {
    let target = if split { crate::pane_view::split_pane(store, pane, crate::panes::Direction::Right).unwrap_or(pane) } else { pane };
    store.link_from.set(Some(target));
    crate::links::follow_href(store, &url.link().to_string());
}

/// "Mark Up...": a picture's board in the board view over the pane; on an
/// embed, the board node itself (there is never a second copy of its marks).
fn mark_up(store: AppStore, pane: PaneId, hover: &PictureHover) {
    if let Some(url) = &hover.embed {
        open_embed(store, pane, url, false);
        return;
    }
    let Some(active) = untracked(|| store.pane(pane).active_edit.get()) else { return };
    store.pane(pane).picture.set(None);
    crate::boards::start_markup(store, pane, active.store_id, active.node_id, &hover.src, hover.pos, hover.board.clone());
}

/// The controls' eye: show or hide the markup of what they are over, for
/// everyone (contract section 5). An embed writes the board node itself.
fn toggle(store: AppStore, pane: PaneId) {
    let Some(hover) = untracked(|| store.pane(pane).picture.get()) else { return };
    let Some((_, visible)) = hover.marks else { return };
    if let Some(url) = &hover.embed {
        let target = EMBEDS.with(|embeds| match &embeds.borrow().get(&(url.store, url.node))?.state {
            EmbedState::Live { store_id, node_id, doc: Some(doc) } => Some((*store_id, *node_id, Board::of_node(doc))),
            _ => None,
        });
        let Some((store_id, node_id, board)) = target else { return };
        if !store.node_access(store_id, node_id).allows_write() {
            crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
            return;
        }
        let author = crate::shares::author_in(store, store_id, node_id);
        let now = crate::boards::now_stamp();
        if let Some(update) = board.edit(Stamp::new(author.as_ref(), &now), |t| t.set_markup_visible(!visible)).update {
            crate::boards::send_node_update(store, store_id, node_id, update);
        }
        return;
    }
    let Some(board_id) = hover.board.as_deref() else { return };
    let Some(active) = untracked(|| store.pane(pane).active_edit.get()) else { return };
    if !store.node_access(active.store_id, active.node_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    let update = HOSTS.with(|hosts| {
        let hosts = hosts.borrow();
        let host = hosts.get(&(active.store_id, active.node_id))?;
        let author = crate::shares::author_in(store, active.store_id, active.node_id);
        let now = crate::boards::now_stamp();
        Board::in_text(&host.doc, board_id).edit(Stamp::new(author.as_ref(), &now), |t| t.set_markup_visible(!visible)).update
    });
    if let Some(update) = update {
        crate::boards::send_node_update(store, active.store_id, active.node_id, update);
    }
}

// ── Convert to Node ────────────────────────────────────────────────────

/// A conversion waiting for its node.
struct Convert {
    pane: PaneId,
    host: (StoreId, NodeId),
    src: String,
    board: Option<String>,
    pos: usize,
    title: String,
}

/// "Convert to Node" (docs/CANVAS_CONTRACT.md): a canvas node under the note
/// with a copy of the canvas in the text (naming the same blob), and the
/// canvas in the text replaced by an embed of it, in one edit of the text.
fn convert(store: AppStore, pane: PaneId, hover: &PictureHover) {
    let Some(active) = untracked(|| store.pane(pane).active_edit.get()) else { return };
    if !store.node_access(active.store_id, active.node_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    let blob = BlobUrl::parse(&hover.src);
    if blob.is_none() && !hover.is_picture() {
        return;
    }
    let raw = hover.board.as_deref().and_then(|b| {
        HOSTS.with(|hosts| hosts.borrow().get(&(active.store_id, active.node_id)).and_then(|h| Board::in_text(&h.doc, b).read()))
    });
    let board = match raw {
        Some(raw) => {
            let author = crate::shares::author_in(store, active.store_id, active.node_id);
            let now = crate::boards::now_stamp();
            Board::standalone().edit(Stamp::new(author.as_ref(), &now), |t| t.copy_from(&raw)).update
        }
        None => match &blob {
            Some(blob) => {
                let size = crate::pictures::held_bytes(blob)
                    .and_then(|bytes| Picture::decode(&bytes).ok())
                    .map(|p| (p.width(), p.height()))
                    .unwrap_or((hover.rect.2.max(1.0) as u32, hover.rect.3.max(1.0) as u32));
                crate::boards::new_board(Some((blob, size.0, size.1)))
            }
            None => None,
        },
    };
    let Some(board) = board else {
        crate::events::show_notice(store, CONVERT_FAILED.to_string());
        return;
    };
    let fallback = if blob.is_some() { "Picture" } else { "Canvas" };
    let title = if hover.alt.trim().is_empty() { fallback.to_string() } else { hover.alt.clone() };
    let icon = if blob.is_some() { crate::boards::PHOTO_ICON } else { crate::boards::WHITEBOARD_ICON };
    let request = NEXT_REQUEST.with(|n| n.replace(n.get() + 1));
    CONVERTS.with(|c| {
        c.borrow_mut().insert(
            request,
            Convert { pane, host: (active.store_id, active.node_id), src: hover.src.clone(), board: hover.board.clone(), pos: hover.pos, title: title.clone() },
        )
    });
    store.pane(pane).picture.set(None);
    crate::boards::create_board_node(store, active.store_id, active.node_id, &title, icon, board, Some(request));
}

/// A node made for a request of this module: the picture it was made from
/// becomes an embed of it.
pub(crate) fn node_created(store: AppStore, request: u64, store_id: StoreId, node_id: NodeId) {
    let Some(job) = CONVERTS.with(|c| c.borrow_mut().remove(&request)) else { return };
    let holds = untracked(|| store.pane(job.pane).active_edit.get()).is_some_and(|e| (e.store_id, e.node_id) == job.host);
    if !holds {
        crate::events::show_notice(store, "The note was closed before the canvas node was made; the picture was left as it was.".to_string());
        return;
    }
    let url = BoardUrl::new(store_id, node_id);
    let replaced = crate::editor::editor(job.pane).update(move |state| {
        let pos = find_image(&state.doc, &job.src, job.board.as_deref(), job.pos)?;
        let mut tr = state.tr();
        tr.step(Box::new(SetNodeAttrStep::new(pos, "src", AttrValue::from(url.to_string())))).ok()?;
        tr.step(Box::new(SetNodeAttrStep::new(pos, "alt", AttrValue::from(job.title.as_str())))).ok()?;
        tr.step(Box::new(SetNodeAttrStep { pos, attr: BOARD_ATTR.into(), value: None })).ok()?;
        Some(tr)
    });
    if !replaced {
        crate::events::show_notice(store, "The picture was no longer in the text, so it was not replaced.".to_string());
    }
}

// ── The controls' view ──────────────────────────────────────────────────────

/// One of a picture's eight resize grips: its corners and the middles of its
/// sides. Every one keeps the picture's proportions (a picture keeps only a
/// width), so a side's grip sizes it as a corner's does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grip {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
}

impl Grip {
    fn north(self) -> bool {
        matches!(self, Grip::NW | Grip::N | Grip::NE)
    }

    fn west(self) -> bool {
        matches!(self, Grip::NW | Grip::W | Grip::SW)
    }

    fn class(self) -> &'static str {
        match self {
            Grip::NW => "pimble-picture-overlay__resize pimble-picture-overlay__resize--nw",
            Grip::N => "pimble-picture-overlay__resize pimble-picture-overlay__resize--n",
            Grip::NE => "pimble-picture-overlay__resize pimble-picture-overlay__resize--ne",
            Grip::E => "pimble-picture-overlay__resize pimble-picture-overlay__resize--e",
            Grip::SE => "pimble-picture-overlay__resize pimble-picture-overlay__resize--se",
            Grip::S => "pimble-picture-overlay__resize pimble-picture-overlay__resize--s",
            Grip::SW => "pimble-picture-overlay__resize pimble-picture-overlay__resize--sw",
            Grip::W => "pimble-picture-overlay__resize pimble-picture-overlay__resize--w",
        }
    }

    /// The width a drag from `(fx, fy)` to `(px, py)` gives a picture `w` by
    /// `h`: a side's grip by its own axis, a corner's by whichever axis the
    /// pointer moved further along (the height's move scaled to a width).
    fn width(self, (w, h): (f32, f32), (fx, fy): (f32, f32), (px, py): (f32, f32)) -> f32 {
        let dx = if self.west() { fx - px } else { px - fx };
        let dy = if self.north() { fy - py } else { py - fy };
        let dy_as_w = if h > 0.0 { dy * w / h } else { 0.0 };
        let dw = match self {
            Grip::E | Grip::W => dx,
            Grip::N | Grip::S => dy_as_w,
            _ if dx.abs() >= dy_as_w.abs() => dx,
            _ => dy_as_w,
        };
        w + dw
    }
}

/// One resize grip of pane `pane`'s picture controls.
fn resize_grip(
    __scope: &mut RenderScope,
    store: AppStore,
    pane: PaneId,
    resizing: Signal<Option<(f32, Grip)>>,
    read_only: impl Fn() -> bool + Copy + 'static,
    grip: Grip,
) -> NodeHandle {
    let state = store.pane(pane);
    rsx! {
        div {
            class: grip.class(),
            title: "Drag to resize",
            style: {move || if read_only() { "display: none;" } else { "" }},
            onmouseenter: move || {
                OVER.with(|o| o.borrow_mut()[pane.index()] = true);
                cancel_hide(pane);
            },
            onmouseleave: move || {
                if untracked(|| resizing.get()).is_none() {
                    OVER.with(|o| o.borrow_mut()[pane.index()] = false);
                    hide_later(store, pane);
                }
            },
            onmousedown: move || {
                let Some(hover) = untracked(|| state.picture.get()) else { return };
                if untracked(read_only) || hover.rect.2 <= 0.0 {
                    return;
                }
                let click = rinch::core::get_click_context();
                let from = (click.mouse_x, click.mouse_y);
                let size = (hover.rect.2, hover.rect.3);
                let width = size.0;
                let widest = text_bounds(pane)
                    .map(|b| untracked(|| b.get().width))
                    .filter(|w| *w > 0.0)
                    .map_or(MAX_TEXT_PX, |w| (w - 64.0).max(MIN_PICTURE_PX))
                    .max(width);
                // The controls stay up through the drag, which leaves the
                // picture as soon as it narrows it.
                OVER.with(|o| o.borrow_mut()[pane.index()] = true);
                cancel_hide(pane);
                resizing.set(Some((width, grip)));
                let done = move || {
                    resizing.set(None);
                    OVER.with(|o| o.borrow_mut()[pane.index()] = false);
                    hide_later(store, pane);
                };
                let hover_end = hover.clone();
                rinch::core::Drag::absolute()
                    .on_move(move |px, py| resizing.set(Some((grip.width(size, from, (px, py)).clamp(MIN_PICTURE_PX, widest), grip))))
                    .on_end(move |_, _| {
                        if let Some((to, _)) = untracked(|| resizing.get()).filter(|(to, _)| (to - width).abs() >= 1.0) {
                            write_width(pane, &hover_end.src, hover_end.board.as_deref(), hover_end.pos, to.round() as i64);
                        }
                        done();
                    })
                    .on_cancel(move |_, _| done())
                    .start();
            },
        }
    }
}

/// Pane `pane`'s floating controls over the picture under the pointer:
/// rendered once per pane slot, positioned over the picture.
pub(crate) fn render_controls(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    APP.with(|a| a.set(Some(store)));
    let state = store.pane(pane);
    let menu_open = Signal::new(false);
    // The width a grip's drag would give the picture, and the grip, while
    // one is under way.
    let resizing: Signal<Option<(f32, Grip)>> = Signal::new(None);
    let read_only = move || {
        state
            .active_edit
            .get()
            .is_some_and(|held| !store.node_access_tracked(held.store_id, held.node_id).allows_write())
    };
    let is_picture = move || state.picture.get().is_some_and(|h| h.is_picture());
    let is_embed = move || state.picture.get().is_some_and(|h| h.embed.is_some());
    let with_hover = move |f: fn(AppStore, PaneId, &PictureHover)| {
        move || {
            menu_open.set(false);
            OVER.with(|o| o.borrow_mut()[pane.index()] = false);
            if let Some(hover) = untracked(|| state.picture.get()) {
                f(store, pane, &hover);
            }
        }
    };
    rsx! {
        div {
            class: {move || if resizing.get().is_some() { "pimble-picture-overlay pimble-picture-overlay--resizing" } else { "pimble-picture-overlay" }},
            style: {move || match state.picture.get() {
                // A grip's drag shows the size the picture will take,
                // keeping its proportions and the side across from the grip
                // in place; it takes it on release.
                Some(PictureHover { rect: (x, y, w, h), .. }) if resizing.get().is_some() && w > 0.0 => {
                    let (nw, grip) = resizing.get().unwrap_or((w, Grip::SE));
                    let nh = h * nw / w;
                    let left = if grip.west() { x + w - nw } else { x };
                    let top = if grip.north() { y + h - nh } else { y };
                    format!("left: {left:.1}px; top: {top:.1}px; width: {nw:.1}px; height: {nh:.1}px;")
                }
                Some(PictureHover { rect: (x, y, w, h), .. }) if !state.markup.get() => {
                    // Inside the text's box: a tall picture scrolled half
                    // out of view keeps its controls in sight.
                    let (top, bottom) = match text_bounds(pane).map(|b| b.get()) {
                        Some(b) if b.height > 0.0 => (y.max(b.y), (y + h).min(b.y + b.height)),
                        _ => (y, y + h),
                    };
                    if bottom - top < 24.0 {
                        "display: none;".to_string()
                    } else {
                        format!("left: {x:.1}px; top: {top:.1}px; width: {w:.1}px; height: {:.1}px;", bottom - top)
                    }
                }
                _ => "display: none;".to_string(),
            }},
            {resize_grip(__scope, store, pane, resizing, read_only, Grip::NW)}
            {resize_grip(__scope, store, pane, resizing, read_only, Grip::N)}
            {resize_grip(__scope, store, pane, resizing, read_only, Grip::NE)}
            {resize_grip(__scope, store, pane, resizing, read_only, Grip::E)}
            {resize_grip(__scope, store, pane, resizing, read_only, Grip::SE)}
            {resize_grip(__scope, store, pane, resizing, read_only, Grip::S)}
            {resize_grip(__scope, store, pane, resizing, read_only, Grip::SW)}
            {resize_grip(__scope, store, pane, resizing, read_only, Grip::W)}
            div {
                class: "pimble-picture-controls",
                onmouseenter: move || {
                    OVER.with(|o| o.borrow_mut()[pane.index()] = true);
                    cancel_hide(pane);
                },
                onmouseleave: move || {
                    OVER.with(|o| o.borrow_mut()[pane.index()] = false);
                    if !untracked(|| menu_open.get()) {
                        hide_later(store, pane);
                    }
                },
                div {
                    class: {move || match state.picture.get().and_then(|h| h.marks) {
                        Some((_, false)) => "pimble-board__markup pimble-board__markup--hidden pimble-picture-controls__eye",
                        _ => "pimble-board__markup pimble-board__markup--shown pimble-picture-controls__eye",
                    }},
                    style: {move || match state.picture.get().and_then(|h| h.marks) {
                        // A picture without markup has no eye; hidden markup keeps it.
                        Some((count, visible)) if count > 0 || !visible => "",
                        _ => "display: none;",
                    }},
                    title: {move || if read_only() { "You can read this, not change it." } else { "Show or hide the markup, for everyone" }},
                    onclick: move || {
                        if !untracked(read_only) {
                            toggle(store, pane);
                        }
                    },
                    span {
                        class: "pimble-board__markup-icon",
                        if state.picture.get().and_then(|h| h.marks).is_none_or(|(_, visible)| visible) {
                            IconGlyph { name: TablerIcon::Eye.name().to_string() }
                        } else {
                            IconGlyph { name: TablerIcon::EyeOff.name().to_string() }
                        }
                    }
                    span {
                        {move || match state.picture.get().and_then(|h| h.marks) {
                            Some((count, true)) => format!("Markup · {count}"),
                            Some((_, false)) => "Markup hidden".to_string(),
                            None => String::new(),
                        }}
                    }
                }
                DropdownMenu {
                    opened_fn: move || menu_open.get(),
                    on_close: move || {
                        menu_open.set(false);
                        if !OVER.with(|o| o.borrow()[pane.index()]) {
                            hide_later(store, pane);
                        }
                    },
                    DropdownMenuTarget {
                        div {
                            class: "pimble-board__markup pimble-board__markup--shown pimble-picture-controls__menu",
                            title: "Picture menu",
                            onclick: move || menu_open.update(|v| *v = !*v),
                            span {
                                class: "pimble-board__markup-icon",
                                IconGlyph { name: TablerIcon::DotsVertical.name().to_string() }
                            }
                        }
                    }
                    DropdownMenuDropdown {
                        DropdownMenuItem {
                            left_section: TablerIcon::Pencil,
                            disabled: {move || is_picture() && read_only()},
                            onclick: with_hover(mark_up),
                            "Mark Up..."
                        }
                        DropdownMenuItem {
                            left_section: TablerIcon::Photo,
                            disabled: {move || !is_picture() || read_only()},
                            onclick: with_hover(convert),
                            "Convert to Node"
                        }
                        DropdownMenuItem {
                            left_section: TablerIcon::ExternalLink,
                            disabled: {move || !is_embed()},
                            onclick: with_hover(|store, pane, hover| {
                                if let Some(url) = &hover.embed {
                                    open_embed(store, pane, url, false);
                                }
                            }),
                            "Open Canvas"
                        }
                        DropdownMenuItem {
                            left_section: TablerIcon::LayoutColumns,
                            disabled: {move || !is_embed() || !store.tiling.with(|tiling| tiling.can_split())},
                            onclick: with_hover(|store, pane, hover| {
                                if let Some(url) = &hover.embed {
                                    open_embed(store, pane, url, true);
                                }
                            }),
                            "Open Canvas in Split View"
                        }
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::protocol::{BackendEvent, BackendHandle};
    use crossbeam_channel::bounded;

    const P: [PaneId; crate::panes::MAX_PANES] = PaneId::ALL;

    fn store_with_commands() -> (AppStore, crossbeam_channel::Receiver<BackendCommand>) {
        let store = AppStore::new();
        let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(512);
        let (_event_tx, event_rx) = bounded::<BackendEvent>(1);
        store.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
        crate::links::set_app_store(store);
        (store, cmd_rx)
    }

    /// A note holding one picture, open in pane `pane`.
    fn note_with_picture(store: AppStore, pane: PaneId) -> (StoreId, NodeId, String) {
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, pane, store_id, node_id, &[]);
        let src = BlobUrl::new(store_id, pimble_core::BlobId::new()).to_string();
        assert!(crate::editor::editor(pane).insert_image(&src, "shot"));
        (store_id, node_id, src)
    }

    fn atoms(pane: PaneId) -> Vec<(usize, Attrs)> {
        images(&crate::editor::editor(pane).doc())
    }

    fn broadcasts(sent: &[BackendCommand], node: NodeId) -> usize {
        sent.iter().filter(|c| matches!(c, BackendCommand::BroadcastChanges { node_id, .. } if *node_id == node)).count()
    }

    /// "Mark Up..." on a picture (contract section 2): opening the board
    /// view writes nothing; the first mark sends the board, writes the
    /// atom's `data-board` and the mark; the note's boards here follow, and
    /// "Done" goes back to the text.
    #[test]
    fn the_first_mark_mints_the_board_and_names_it_on_the_picture() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id, src) = note_with_picture(store, P[0]);
        let (pos, attrs) = atoms(P[0]).remove(0);
        assert_eq!(attrs.get_str(BOARD_ATTR), None);
        let _ = commands.try_iter().count();

        crate::boards::start_markup(store, P[0], store_id, node_id, &src, pos, None);
        assert!(store.pane(P[0]).markup.get());
        assert!(commands.try_iter().all(|c| !matches!(c, BackendCommand::BroadcastChanges { .. })), "opening writes nothing");
        assert!(HOSTS.with(|h| h.borrow()[&(store_id, node_id)].doc.data_json().as_object().is_none_or(|d| d.keys().all(|k| !k.starts_with(pimble_board::TEXT_BOARD_PREFIX)))));

        crate::boards::mark_for_test(store, P[0]);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(broadcasts(&sent, node_id), 3, "the board, the atom's attribute, the mark");
        let board = atoms(P[0])[0].1.get_str(BOARD_ATTR).map(str::to_string).expect("the picture names its board");
        let snapshot = board_snapshot(store_id, node_id, &board).expect("the note's board here");
        assert_eq!(snapshot.markup_count(), 1);
        assert_eq!(snapshot.elements.iter().filter(|e| e.raw.kind == Kind::Picture).count(), 1);

        // A second mark is only a mark.
        crate::boards::mark_for_test(store, P[0]);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(broadcasts(&sent, node_id), 1);

        crate::boards::end_markup(store, P[0]);
        assert!(!store.pane(P[0]).markup.get() && !store.pane(P[0]).show_board.get());
        assert!(untracked(|| store.pane(P[0]).active_edit.get()).is_some(), "the text is still open");

        // Marked up again: the same board, nothing made.
        crate::boards::start_markup(store, P[0], store_id, node_id, &src, pos, Some(board.clone()));
        crate::boards::mark_for_test(store, P[0]);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(broadcasts(&sent, node_id), 1);
        assert_eq!(board_snapshot(store_id, node_id, &board).unwrap().markup_count(), 3);
        crate::editor::stop_editing(store, P[0]);
        assert!(!store.pane(P[0]).markup.get());
    }

    /// The first broadcast of node `node` in `sent`, applied to an empty
    /// document: what a peer gets first.
    fn first_broadcast(sent: &[BackendCommand], node: NodeId) -> NodeDoc {
        use base64::Engine;
        let changes = sent
            .iter()
            .find_map(|c| match c {
                BackendCommand::BroadcastChanges { node_id, changes, .. } if *node_id == node => Some(changes.clone()),
                _ => None,
            })
            .expect("a broadcast");
        let mut doc = NodeDoc::new();
        doc.apply_update(&base64::engine::general_purpose::STANDARD.decode(changes).unwrap()).unwrap();
        doc
    }

    /// A picture put into text is a canvas from the start
    /// (docs/CANVAS_CONTRACT.md section 3): the canvas goes out first, with
    /// the picture locked in a layer of its own and "Layer 1" above it; then
    /// the atom, naming it. Nothing is drawn on it, so the text shows the
    /// picture itself, and the first mark is only the mark.
    #[test]
    fn a_picture_put_into_text_is_a_canvas_from_the_start() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, P[2], store_id, node_id, &[]);
        let blob = BlobUrl::new(store_id, pimble_core::BlobId::new());
        let _ = commands.try_iter().count();

        assert!(insert_canvas(store, P[2], store_id, node_id, Some((&blob, 320, 200)), "sunset", None));
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(broadcasts(&sent, node_id), 2, "the canvas, then the text");
        let (pos, attrs) = atoms(P[2]).remove(0);
        let board = attrs.get_str(BOARD_ATTR).expect("the picture names its canvas").to_string();
        assert_eq!(attrs.get_str("src"), Some(blob.to_string().as_str()));
        assert_eq!(attrs.get_str("alt"), Some("sunset"));
        assert!(Board::in_text(&first_broadcast(&sent, node_id), &board).exists(), "the canvas travels before the text names it");

        let snapshot = board_snapshot(store_id, node_id, &board).expect("the note's canvas here");
        let names: Vec<&str> = snapshot.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Picture", "Layer 1"]);
        assert!(snapshot.layers[0].locked && !snapshot.layers[1].locked);
        assert_eq!(snapshot.markup_count(), 0);
        assert!(shows_only_its_picture(&snapshot, &blob), "shown as the picture itself");

        crate::boards::start_markup(store, P[2], store_id, node_id, &blob.to_string(), pos, Some(board.clone()));
        crate::boards::mark_for_test(store, P[2]);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(broadcasts(&sent, node_id), 1, "the first mark is only the mark");
        let snapshot = board_snapshot(store_id, node_id, &board).unwrap();
        assert!(!shows_only_its_picture(&snapshot, &blob), "drawn on: the renderer takes over");
        crate::boards::end_markup(store, P[2]);
        crate::editor::stop_editing(store, P[2]);
    }

    /// Insert > "Canvas" (docs/CANVAS_CONTRACT.md, decision 4): an empty
    /// canvas in the text, drawn as a space to draw in, marked up like any
    /// picture.
    #[test]
    fn insert_canvas_puts_an_empty_canvas_in_the_text() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, P[3], store_id, node_id, &[]);
        store.focus_pane(P[3]);
        let _ = commands.try_iter().count();

        insert_board(store);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(broadcasts(&sent, node_id), 2);
        let (pos, attrs) = atoms(P[3]).remove(0);
        assert_eq!(attrs.get_str("src"), Some(IN_TEXT_SRC));
        let board = attrs.get_str(BOARD_ATTR).expect("it names its canvas").to_string();
        let snapshot = board_snapshot(store_id, node_id, &board).expect("an empty canvas here");
        assert_eq!(snapshot.layers.len(), 1);
        assert!(snapshot.elements.is_empty());
        assert!(matches!(prepare(&marked_src(IN_TEXT_SRC, &board)), Some(Job::Ready(_))), "a placeholder until something is drawn");

        crate::boards::start_markup(store, P[3], store_id, node_id, IN_TEXT_SRC, pos, Some(board.clone()));
        assert!(store.pane(P[3]).markup.get());
        crate::boards::mark_for_test(store, P[3]);
        assert_eq!(board_snapshot(store_id, node_id, &board).unwrap().markup_count(), 1);
        assert!(matches!(prepare(&marked_src(IN_TEXT_SRC, &board)), Some(Job::Draw { .. })));
        crate::boards::end_markup(store, P[3]);
        crate::editor::stop_editing(store, P[3]);
    }

    /// A store's picture pasted with no canvas (copied from text written
    /// before every picture was a canvas) gets a canvas of its own.
    #[test]
    fn a_pasted_bare_picture_gets_a_canvas() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, P[0], store_id, node_id, &[]);
        let mut open = pimble_core::Store::new_local("Notes", std::path::PathBuf::from("/nowhere/notes.pimble"));
        open.id = store_id;
        store.store_data.update(|map| {
            map.insert(store_id, Signal::new(open));
        });
        let blob = BlobUrl::new(store_id, pimble_core::BlobId::new());
        let png = pimble_board::RgbaImage { width: 4, height: 3, data: vec![255; 48] }.encode_png().unwrap();
        crate::pictures::loaded(blob, &Ok(Arc::new(png)));
        let _ = commands.try_iter().count();

        let html = format!("<p><img src=\"{blob}\" alt=\"old\"></p>");
        assert!(crate::editor::editor(P[0]).paste(&rinch_editor_core::PasteContent::new(None, Some(html))));
        let (_, attrs) = atoms(P[0]).remove(0);
        let board = attrs.get_str(BOARD_ATTR).expect("a canvas of its own").to_string();
        let snapshot = board_snapshot(store_id, node_id, &board).expect("made here");
        assert!(shows_only_its_picture(&snapshot, &blob));
        let picture = snapshot.elements.iter().find_map(|e| match &e.raw.geom {
            Geom::Picture(p) => Some((p.px_w, p.px_h)),
            _ => None,
        });
        assert_eq!(picture, Some((4, 3)));
        crate::editor::stop_editing(store, P[0]);
    }

    /// A marked-up picture pasted (contract section 2, "Copy and paste"):
    /// it gets a board of its own, a copy, even in the same note.
    #[test]
    fn a_pasted_marked_up_picture_brings_a_copy_of_its_board() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id, src) = note_with_picture(store, P[1]);
        let pos = atoms(P[1])[0].0;
        crate::boards::start_markup(store, P[1], store_id, node_id, &src, pos, None);
        crate::boards::mark_for_test(store, P[1]);
        crate::boards::end_markup(store, P[1]);
        let board = atoms(P[1])[0].1.get_str(BOARD_ATTR).unwrap().to_string();
        let _ = commands.try_iter().count();
        // Pictures of a store open here are pasted (any other is left out).
        let mut open = pimble_core::Store::new_local("Notes", std::path::PathBuf::from("/nowhere/notes.pimble"));
        open.id = store_id;
        store.store_data.update(|map| {
            map.insert(store_id, Signal::new(open));
        });

        let html = format!("<p><img src=\"{src}\" alt=\"shot\" data-board=\"{board}\"></p>");
        assert!(crate::editor::editor(P[1]).paste(&rinch_editor_core::PasteContent::new(None, Some(html))));
        let boards: Vec<String> = atoms(P[1]).iter().filter_map(|(_, a)| a.get_str(BOARD_ATTR).map(str::to_string)).collect();
        assert_eq!(boards.len(), 2, "{boards:?}");
        assert_ne!(boards[0], boards[1], "two pictures, two boards");
        for b in &boards {
            assert_eq!(board_snapshot(store_id, node_id, b).map(|s| s.markup_count()), Some(1), "{b}");
        }
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(broadcasts(&sent, node_id) >= 2, "the pasted text and the copied board both travel");

        // A board this window does not hold cannot come along: the picture
        // goes in as it came.
        let html = format!("<p><img src=\"{src}\" data-board=\"0123456789abcdef\"></p>");
        assert!(crate::editor::editor(P[1]).paste(&rinch_editor_core::PasteContent::new(None, Some(html))));
        assert_eq!(atoms(P[1]).len(), 3);
        assert_eq!(atoms(P[1]).iter().filter(|(_, a)| a.get_str(BOARD_ATTR).is_some()).count(), 2);
        crate::editor::stop_editing(store, P[1]);
    }

    /// "Convert to Node": an image node under the note with the
    /// picture's markup, and the picture replaced by an embed of it.
    #[test]
    fn converting_a_picture_makes_an_image_node_and_embeds_it() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id, src) = note_with_picture(store, P[2]);
        let pos = atoms(P[2])[0].0;
        crate::boards::start_markup(store, P[2], store_id, node_id, &src, pos, None);
        crate::boards::mark_for_test(store, P[2]);
        crate::boards::end_markup(store, P[2]);
        let board = atoms(P[2])[0].1.get_str(BOARD_ATTR).unwrap().to_string();
        let _ = commands.try_iter().count();

        let hover = PictureHover {
            pos,
            src: src.clone(),
            alt: "shot".into(),
            board: Some(board),
            embed: None,
            rect: (0.0, 0.0, 100.0, 80.0),
            marks: Some((1, true)),
        };
        convert(store, P[2], &hover);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        let (request, content) = sent
            .iter()
            .find_map(|c| match c {
                BackendCommand::CreateNode { store_id: s, parent_id: Some(p), title, init: Some(init), .. } if (*s, *p) == (store_id, node_id) => {
                    assert_eq!(title, "shot");
                    assert_eq!(init.node_type, pimble_core::node_types::BOARD);
                    Some((init.request.expect("a request"), init.content.clone()))
                }
                _ => None,
            })
            .expect("an image node is made under the note");
        let mut made = NodeDoc::new();
        made.apply_update(&content).unwrap();
        let snapshot = Board::of_node(&made).snapshot();
        assert_eq!(snapshot.markup_count(), 1, "with a copy of the picture's markup");
        assert!(snapshot.elements.iter().any(|e| matches!(&e.raw.geom, Geom::Picture(g) if g.src == src)), "the same blob");

        let new_node = NodeId::new();
        node_created(store, request, store_id, new_node);
        let (_, attrs) = atoms(P[2]).remove(0);
        assert_eq!(attrs.get_str("src"), Some(BoardUrl::new(store_id, new_node).to_string().as_str()));
        assert_eq!(attrs.get_str(BOARD_ATTR), None);
        assert_eq!(attrs.get_str("alt"), Some("shot"));
        // An edit of the text like any other: undo takes the embed back.
        let embed = BoardUrl::new(store_id, new_node).to_string();
        assert!(crate::editor::editor(P[2]).command("undo"));
        assert!(atoms(P[2]).iter().all(|(_, a)| a.get_str("src") != Some(embed.as_str())));
        crate::editor::stop_editing(store, P[2]);
    }

    /// An embedded board (contract section 2, "Showing"): resolved through
    /// the server, held (subscribed, fetched) while shown, drawn when its
    /// document arrives, and let go of when no text shows it.
    #[test]
    fn an_embed_holds_its_board_while_it_is_shown() {
        let (store, commands) = store_with_commands();
        let (store_id, note) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, P[3], store_id, note, &[]);
        let board_node = NodeId::new();
        let url = BoardUrl::new(store_id, board_node);
        assert!(crate::editor::editor(P[3]).insert_image(&url.to_string(), "Plan"));
        let _ = commands.try_iter().count();

        let src = url.to_string();
        wanted(&src);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(sent.iter().any(|c| matches!(c, BackendCommand::ResolveLink { url: u, .. } if *u == url.link())));
        assert!(link_resolved(store, &url.link(), 0, &LinkResolution::Live { store_id, node_id: board_node }));
        assert!(embed_holds(store_id, board_node) && store.is_editing(store_id, board_node));
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(sent.iter().any(|c| matches!(c, BackendCommand::SubscribeNodeChanges { node_id, .. } if *node_id == board_node)));
        assert!(sent.iter().any(|c| matches!(c, BackendCommand::ReconcileNodeContent { node_id, .. } if *node_id == board_node)));

        // Its document arrives: drawn.
        let mut doc = NodeDoc::new();
        doc.init(pimble_core::node_types::BOARD, "Plan", None, "2026-10-10T10:00:00.000Z").unwrap();
        doc.apply_update(&crate::boards::new_board(None).unwrap()).unwrap();
        let b = Board::of_node(&doc);
        b.add_element(Stamp::new(None, "2026-10-10T10:00:01.000Z"), NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 40.0, 30.0)))).unwrap();
        apply_reconcile(store_id, board_node, &doc.save());
        let png = rendered(|r| r.images.get(&src).cloned()).expect("drawn");
        assert!(png.starts_with(b"\x89PNG"));

        // A board nobody may read: one sentence, never its pixels.
        let hidden = BoardUrl::new(store_id, NodeId::new());
        wanted(&hidden.to_string());
        assert!(link_resolved(store, &hidden.link(), 0, &LinkResolution::NoAccess));
        assert_eq!(rendered(|r| r.images.get(&hidden.to_string()).cloned()), Some(placeholder(NO_ACCESS)));

        // Gone from the text: let go of.
        let _ = commands.try_iter().count();
        let at = atoms(P[3])[0].0;
        assert!(crate::editor::editor(P[3]).update(move |state| {
            let mut tr = state.tr();
            tr.delete(at, at + 1).ok()?;
            Some(tr)
        }));
        sweep(store);
        assert!(!embed_holds(store_id, board_node));
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(sent.iter().any(|c| matches!(c, BackendCommand::UnsubscribeNodeChanges { node_id, .. } if *node_id == board_node)));
        crate::editor::stop_editing(store, P[3]);
    }

    #[test]
    fn sources_are_told_apart() {
        let blob = BlobUrl::new(StoreId::new(), pimble_core::BlobId::new());
        let marked = marked_src(&blob.to_string(), "a1b2");
        assert!(marked.ends_with("#canvas=a1b2"));
        assert_eq!(parse_source(&marked), Some(Source::Marked { blob: Some(blob), board: "a1b2".into() }));
        assert!(is_marked_src(&marked));
        let before = format!("{blob}#board=a1b2");
        assert_eq!(parse_source(&before), Some(Source::Marked { blob: Some(blob), board: "a1b2".into() }), "the fragment of before canvases is still read");
        assert!(is_marked_src(&before));
        let empty = marked_src(IN_TEXT_SRC, "c3");
        assert_eq!(parse_source(&empty), Some(Source::Marked { blob: None, board: "c3".into() }));
        assert_eq!(parse_source(IN_TEXT_SRC), None);
        let embed = BoardUrl::new(StoreId::new(), NodeId::new());
        assert_eq!(parse_source(&embed.to_string()), Some(Source::Embed(embed)));
        assert_eq!(parse_source(&blob.to_string()), None, "a plain picture is the blob loader's");
        assert_eq!(parse_source(&format!("{blob}#board=<x>")), None);
    }
}

#[cfg(test)]
mod grip_tests {
    use super::Grip;

    #[test]
    fn every_grip_grows_the_picture_away_from_the_side_across_from_it() {
        let size = (200.0, 100.0);
        let from = (0.0, 0.0);
        // Outward is right for the east grips, left for the west ones, down
        // for the south ones and up for the north ones.
        assert_eq!(Grip::E.width(size, from, (30.0, 0.0)), 230.0);
        assert_eq!(Grip::W.width(size, from, (-30.0, 0.0)), 230.0);
        assert_eq!(Grip::W.width(size, from, (30.0, 0.0)), 170.0, "inward narrows it");
        // A side's height move is scaled to a width: 10px of a 100px-high
        // picture is a tenth, 20px of its 200px width.
        assert_eq!(Grip::S.width(size, from, (500.0, 10.0)), 220.0, "the side's own axis only");
        assert_eq!(Grip::N.width(size, from, (0.0, -10.0)), 220.0);
        // A corner follows whichever axis moved further.
        assert_eq!(Grip::SE.width(size, from, (5.0, 10.0)), 220.0);
        assert_eq!(Grip::NW.width(size, from, (-40.0, -10.0)), 240.0);
        assert_eq!(Grip::SW.width(size, from, (-5.0, 10.0)), 220.0);
    }
}
