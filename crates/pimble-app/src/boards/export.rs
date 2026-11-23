//! A board as a picture, out of Pimble (docs/IMAGE_MARKUP_CONTRACT.md
//! section 8, "Export"): Copy Picture, Save Picture As (PNG, JPEG, SVG; what
//! the board shows, or with all markup), Save Original As (a selected
//! picture's own bytes) and Flatten into New Image (a new image node; the
//! marks are not carried over). Every one is rendered by `pimble-board`,
//! the renderer the view paints with, so a copy looks as the board does.
//!
//! What is rendered: a selected frame is its frame; another selection is
//! just those elements, trimmed to their pixels; nothing selected is the
//! board's page, or its visible extent trimmed to visible pixels.

use pimble_board::{render_area, render_svg, selection_bounds, Rendered};

use super::*;
use crate::toolbar::Picked;

/// One board unit to one pixel: an image node exports at its picture's size.
const EXPORT_SCALE: f32 = 1.0;

/// A picture format to save in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    Png,
    Jpeg,
    Svg,
}

impl Format {
    fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Svg => "svg",
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Svg => "image/svg+xml",
        }
    }
}

/// What an export of pane `pane`'s board covers: the snapshot to draw (the
/// document as it shows, or with every layer and mark when `all`), the board
/// rectangle, the elements to draw (`None`: all), whether to trim to the
/// visible pixels, and the background.
struct Scope {
    snapshot: BoardSnapshot,
    rect: Rect,
    only: Option<HashSet<String>>,
    trim: bool,
    background: Option<Color>,
}

fn scope(pane: PaneId, all: bool) -> Option<Scope> {
    with_session(pane, |s| {
        // Exports are never colored by author: that is a way of looking.
        let options = ViewOptions { color_by_author: false, show_all: all, card_labels: s.options.card_labels.clone(), ..ViewOptions::default() };
        let snapshot = BoardSnapshot::build_with(s.board.read(), options);
        let selected: Vec<String> = s.selected.iter().filter(|id| snapshot.get(id).is_some()).cloned().collect();
        let page_fill = snapshot.page.as_ref().map(|p| p.fill.unwrap_or(Color::WHITE));
        if selected.len() == 1 && snapshot.get(&selected[0]).is_some_and(|e| e.raw.kind == Kind::Frame) {
            let e = snapshot.get(&selected[0])?;
            let r = e.pivot_bounds;
            let rect = Rect::new(r.x + e.world.tx, r.y + e.world.ty, r.w * e.world.sx, r.h * e.world.sy);
            return Some(Scope { snapshot, rect, only: None, trim: false, background: Some(page_fill.unwrap_or(Color::WHITE)) });
        }
        if !selected.is_empty() {
            let rect = selection_bounds(&snapshot, &selected)?;
            let only: HashSet<String> = selected.into_iter().collect();
            return Some(Scope { snapshot, rect, only: Some(only), trim: true, background: None });
        }
        let rect = snapshot.render_rect()?;
        Some(Scope { snapshot, rect, only: None, trim: page_fill.is_none(), background: page_fill })
    })
    .flatten()
}

fn rendered(scope: &Scope) -> Option<Rendered> {
    render_area(&scope.snapshot, &picture_source(), scope.rect, EXPORT_SCALE, scope.background, scope.only.as_ref(), scope.trim)
}

/// The board's title, for file names.
fn title(store: AppStore, pane: PaneId) -> String {
    title_of(store, with_session(pane, |s| (s.store_id, s.node_id)))
}

/// Node `held`'s title, made fit for a file name ("Canvas" when it has none).
fn title_of(store: AppStore, held: Option<(StoreId, NodeId)>) -> String {
    let title = held
        .and_then(|(sid, nid)| store.get_node_signal(sid, nid))
        .map(|sig| untracked(|| sig.with(|n| n.metadata.title.clone())))
        .unwrap_or_default();
    let clean: String = title.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c }).collect();
    if clean.trim().is_empty() {
        "Canvas".to_string()
    } else {
        clean.trim().to_string()
    }
}

const NOTHING_SHOWS: &str = "There is nothing on this canvas to make a picture of.";

/// Copy Picture (Ctrl+C on a board): a flattened PNG on the clipboard.
pub(crate) fn copy_picture(store: AppStore, pane: PaneId) {
    copy_scope(store, scope(pane, false));
}

/// Copy what `scope` covers as a flattened PNG.
fn copy_scope(store: AppStore, scope: Option<Scope>) {
    copy_scope_as(store, scope, None);
}

/// [`copy_scope`], remembering `html` (the picture as the note's text holds
/// it) beside the pixels, so that pasting this very picture into a note
/// again brings the canvas with its markup, not a flattened copy
/// (`pictures::copied_html_for`).
fn copy_scope_as(store: AppStore, scope: Option<Scope>, html: Option<String>) {
    let Some(picture) = scope.and_then(|s| rendered(&s)) else {
        crate::events::show_notice(store, NOTHING_SHOWS.to_string());
        return;
    };
    let image = picture.image;
    if let Some(html) = html {
        crate::pictures::remember_copy(image.width, image.height, &image.data, html);
    }
    #[cfg(feature = "native")]
    {
        let data = rinch::clipboard::ImageData::new(image.width as usize, image.height as usize, image.data);
        match rinch::clipboard::copy_image(data) {
            Ok(()) => crate::events::show_notice(store, "The picture is on the clipboard.".to_string()),
            Err(e) => crate::events::show_notice(store, format!("The picture could not be copied: {e}")),
        }
    }
    #[cfg(not(feature = "native"))]
    {
        match image.encode_png() {
            Ok(png) => browser::copy_png(store, png),
            Err(e) => crate::events::show_notice(store, format!("The picture could not be copied: {e}")),
        }
    }
}

/// Save Picture As...: the board as `format`, what it shows or with all
/// markup (a one-off copy; the board is not changed).
pub(super) fn save_picture(store: AppStore, pane: PaneId, format: Format, all: bool) {
    save_scope(store, scope(pane, all), format, all, title(store, pane));
}

/// Save what `scope` covers as `format`, named after `title`.
fn save_scope(store: AppStore, scope: Option<Scope>, format: Format, all: bool, title: String) {
    let Some(scope) = scope else {
        crate::events::show_notice(store, NOTHING_SHOWS.to_string());
        return;
    };
    let bytes = match format {
        Format::Svg => render_svg(&scope.snapshot, scope.rect, scope.only.as_ref(), &|src: &str| picture_bytes(src)).into_bytes(),
        Format::Png | Format::Jpeg => {
            let Some(picture) = rendered(&scope) else {
                crate::events::show_notice(store, NOTHING_SHOWS.to_string());
                return;
            };
            let encoded = match format {
                Format::Png => picture.image.encode_png(),
                _ => picture.image.encode_jpeg(90, Color::WHITE),
            };
            match encoded {
                Ok(bytes) => bytes,
                Err(e) => {
                    crate::events::show_notice(store, format!("The picture could not be made: {e}"));
                    return;
                }
            }
        }
    };
    let name = rendered_name(&title, format, all);
    save_bytes(store, &name, format.mime(), format.extension(), bytes);
}

/// Save Original As...: the selected picture's bytes, exactly as stored.
pub(super) fn save_original(store: AppStore, pane: PaneId) {
    let src = with_session(pane, |s| {
        let e = s.selected.first().and_then(|id| s.snapshot().get(id))?;
        match &e.raw.geom {
            Geom::Picture(p) => Some(p.src.clone()),
            _ => None,
        }
    })
    .flatten();
    let Some(src) = src else {
        crate::events::show_notice(store, "Select a picture to save its original.".to_string());
        return;
    };
    let Some(bytes) = picture_bytes(&src) else {
        crate::events::show_notice(store, "This picture has not arrived on this device yet.".to_string());
        return;
    };
    save_original_bytes(store, title(store, pane), bytes);
}

/// Save a picture's bytes as stored, named "<title> (original)" with the
/// extension its own format has, so it never takes a rendered export's name.
fn save_original_bytes(store: AppStore, title: String, bytes: Vec<u8>) {
    let (mime, ext) = match pimble_core::ImageMime::sniff(&bytes) {
        Some(m) => (m.as_str(), m.as_str().trim_start_matches("image/").replace("jpeg", "jpg")),
        None => ("application/octet-stream", "bin".to_string()),
    };
    save_bytes(store, &original_name(&title, &ext), mime, &ext, bytes);
}

/// An original's file name.
pub(super) fn original_name(title: &str, ext: &str) -> String {
    format!("{title} (original).{ext}")
}

/// A rendered export's file name: what the canvas shows, or with all markup.
pub(super) fn rendered_name(title: &str, format: Format, all: bool) -> String {
    let suffix = if all { " (all markup)" } else { "" };
    format!("{title}{suffix}.{}", format.extension())
}

/// Flatten into New Image: what the board shows, rendered once into a new
/// picture and a new image node beside the board (contract section 8: the
/// markup is not carried over; it is a frozen copy).
pub(super) fn flatten(store: AppStore, pane: PaneId) {
    let Some(held) = with_session(pane, |s| (s.store_id, s.node_id)) else { return };
    let Some(picture) = with_session(pane, |_| ()).and_then(|_| {
        let options = with_session(pane, |s| ViewOptions { card_labels: s.options.card_labels.clone(), ..ViewOptions::default() })?;
        let snapshot = with_session(pane, |s| BoardSnapshot::build_with(s.board.read(), options))?;
        let rect = snapshot.render_rect()?;
        let background = snapshot.page.as_ref().map(|p| p.fill.unwrap_or(Color::WHITE));
        render_area(&snapshot, &picture_source(), rect, EXPORT_SCALE, background, None, background.is_none())
    }) else {
        crate::events::show_notice(store, NOTHING_SHOWS.to_string());
        return;
    };
    plant_flattened(store, held, picture, title(store, pane));
}

/// A flattened rendering of a canvas of node `held`, as a new canvas node
/// beside that node, named after `title`.
fn plant_flattened(store: AppStore, held: (StoreId, NodeId), picture: Rendered, title: String) {
    let png = match picture.image.encode_png() {
        Ok(png) => png,
        Err(e) => {
            crate::events::show_notice(store, format!("The picture could not be made: {e}"));
            return;
        }
    };
    let parent = store
        .get_node_signal(held.0, held.1)
        .and_then(|sig| untracked(|| sig.with(|n| n.parent_id)))
        .unwrap_or(held.1);
    let name = format!("{title} (flattened).png");
    add_image_nodes(store, held.0, parent, vec![(Some(name), png)]);
}

// ── A canvas selected in a note's text (docs/TOOLBAR_CONTRACT.md
// "Selected picture") ─────────────────────────────────────────────────────

/// The note holding the selected canvas `picked`.
fn picked_host(store: AppStore, picked: &Picked) -> Option<(StoreId, NodeId)> {
    untracked(|| store.pane(picked.pane).active_edit.get()).map(|a| (a.store_id, a.node_id))
}

/// The selected canvas's board read whole: its markup board in the note, or
/// (a picture not marked up yet) the board its markup would start as.
pub(super) fn picked_raw(store: AppStore, picked: &Picked) -> Option<pimble_board::RawBoard> {
    // An embedded canvas node: that node's board, as this window holds it.
    if let Some(url) = &picked.embed {
        return crate::text_boards::embed_raw(url);
    }
    let (store_id, node_id) = picked_host(store, picked)?;
    if let Some(raw) = picked.board.as_deref().and_then(|b| crate::text_boards::raw_board(store_id, node_id, b)) {
        return Some(raw);
    }
    let blob = BlobUrl::parse(&picked.src)?;
    let bytes = picture_bytes(&picked.src)?;
    let size = Picture::decode(&bytes).map(|p| (p.width(), p.height())).ok()?;
    let doc = NodeDoc::new();
    let board = Board::in_text(&doc, "export");
    build_board(&board, None, Some((&blob, size.0, size.1)));
    board.read()
}

/// Decode now every picture `snapshot` names whose bytes this window holds;
/// false (and they are asked for) when one is not here yet.
fn pictures_ready(snapshot: &BoardSnapshot) -> bool {
    let source = picture_source();
    let mut ready = true;
    for src in snapshot.picture_srcs() {
        if source(&src).is_some() {
            continue;
        }
        match picture_bytes(&src).and_then(|bytes| Picture::decode(&bytes).ok().map(|p| (p, bytes))) {
            Some((picture, bytes)) => seed_picture(&src, Arc::new(picture), bytes),
            None => ready = false,
        }
    }
    if !ready {
        request_wanted_pictures();
    }
    ready
}

const NOT_ARRIVED: &str = "This picture has not arrived on this device yet.";

/// What an export of the selected canvas covers: all of it, as it shows (or
/// with every layer and mark when `all`). `Err` is the sentence to say.
fn picked_scope(store: AppStore, picked: &Picked, all: bool) -> Result<Scope, &'static str> {
    let raw = picked_raw(store, picked).ok_or(NOT_ARRIVED)?;
    let snapshot = BoardSnapshot::build_with(Some(raw), ViewOptions { show_all: all, ..ViewOptions::default() });
    if !pictures_ready(&snapshot) {
        return Err(NOT_ARRIVED);
    }
    // An embed showing one frame: that frame, as a selected frame exports.
    let frame = picked.embed.as_ref().and_then(|u| u.frame.as_deref()).and_then(|id| {
        let e = snapshot.get(id).filter(|e| e.raw.kind == Kind::Frame)?;
        let r = e.pivot_bounds;
        Some(Rect::new(r.x + e.world.tx, r.y + e.world.ty, r.w * e.world.sx, r.h * e.world.sy))
    });
    let page_fill = snapshot.page.as_ref().map(|p| p.fill.unwrap_or(Color::WHITE));
    if let Some(rect) = frame {
        return Ok(Scope { snapshot, rect, only: None, trim: false, background: Some(page_fill.unwrap_or(Color::WHITE)) });
    }
    let rect = snapshot.render_rect().ok_or(NOTHING_SHOWS)?;
    Ok(Scope { snapshot, rect, only: None, trim: page_fill.is_none(), background: page_fill })
}

/// The name an export of the selected canvas starts from: an embedded canvas
/// node's title, or the note's for a canvas of its own.
fn picked_title(store: AppStore, picked: &Picked) -> String {
    match &picked.embed {
        Some(url) => title_of(store, Some((url.store, url.node))),
        None => title_of(store, picked_host(store, picked)),
    }
}

/// The picture the selected canvas is of, as a `src`: a picture's own, or
/// the one picture an embedded canvas holds.
fn picked_original(store: AppStore, picked: &Picked) -> Option<String> {
    if picked.embed.is_none() {
        return BlobUrl::parse(&picked.src).map(|_| picked.src.clone());
    }
    let raw = picked_raw(store, picked)?;
    let mut pictures = raw.elements.iter().filter_map(|e| match &e.geom {
        Geom::Picture(p) => Some(p.src.clone()),
        _ => None,
    });
    match (pictures.next(), pictures.next()) {
        (Some(src), None) => Some(src),
        _ => None,
    }
}

/// The selected canvas's scope, or `None` having said why.
fn picked_scope_said(store: AppStore, picked: &Picked, all: bool) -> Option<Scope> {
    match picked_scope(store, picked, all) {
        Ok(scope) => Some(scope),
        Err(sentence) => {
            crate::events::show_notice(store, sentence.to_string());
            None
        }
    }
}

/// Copy Picture, on the selected canvas.
pub(super) fn copy_picked(store: AppStore, picked: &Picked) {
    if let Some(scope) = picked_scope_said(store, picked, false) {
        copy_scope(store, Some(scope));
    }
}

/// Ctrl+C (and Copy in its right-click menu) on a canvas selected in a
/// note's text: the picture itself on the clipboard, as Copy Picture puts it
/// there, so it pastes into any program. Copied as text it was only the
/// placeholder character U+FFFC wherever the note's html was not read (Will,
/// 2026-11-23). `html` is the selection as the editor copies it, for a paste
/// back into a note.
pub(crate) fn copy_picked_from_text(store: AppStore, picked: &Picked, html: Option<String>) {
    if let Some(scope) = picked_scope_said(store, picked, false) {
        copy_scope_as(store, Some(scope), html);
    }
}

/// Save Picture As... (a PNG of what it shows) on a canvas in a note's text,
/// from the picture's own menu.
pub(crate) fn save_picked_png(store: AppStore, picked: &Picked) {
    save_picked(store, picked, Format::Png, false);
}

/// Save as PNG/JPEG/SVG (or with all markup), on the selected canvas.
pub(super) fn save_picked(store: AppStore, picked: &Picked, format: Format, all: bool) {
    if let Some(scope) = picked_scope_said(store, picked, all) {
        save_scope(store, Some(scope), format, all, picked_title(store, picked));
    }
}

/// Why Save Original As... has nothing to save on the selected canvas, if
/// it has nothing: a canvas that began empty holds no picture of its own,
/// and an embedded canvas has one only when it holds exactly one.
pub(super) fn no_original(store: AppStore, picked: &Picked) -> Option<&'static str> {
    if picked_original(store, picked).is_some() {
        return None;
    }
    Some(if picked.embed.is_some() {
        "This canvas holds no single picture: open it and select one to save its original."
    } else {
        "This canvas began empty: there is no original picture to save."
    })
}

/// Save Original As..., on the selected canvas: its picture's bytes as
/// stored.
pub(super) fn save_picked_original(store: AppStore, picked: &Picked) {
    let Some(src) = picked_original(store, picked) else {
        if let Some(why) = no_original(store, picked) {
            crate::events::show_notice(store, why.to_string());
        }
        return;
    };
    let Some(bytes) = picture_bytes(&src) else {
        crate::events::show_notice(store, NOT_ARRIVED.to_string());
        return;
    };
    save_original_bytes(store, picked_title(store, picked), bytes);
}

/// Flatten into New Canvas, on the selected canvas: a new canvas node beside
/// the note.
pub(super) fn flatten_picked(store: AppStore, picked: &Picked) {
    let Some(held) = picked_host(store, picked) else { return };
    let Some(scope) = picked_scope_said(store, picked, false) else { return };
    let Some(picture) = rendered(&scope) else {
        crate::events::show_notice(store, NOTHING_SHOWS.to_string());
        return;
    };
    // Beside the canvas node an embed shows, or beside the note.
    let beside = picked.embed.as_ref().map(|u| (u.store, u.node)).unwrap_or(held);
    plant_flattened(store, beside, picture, picked_title(store, picked));
}

/// Save `bytes` as a file the person names: a save dialog on the desktop, a
/// download in the browser.
fn save_bytes(store: AppStore, name: &str, mime: &str, extension: &str, bytes: Vec<u8>) {
    #[cfg(feature = "native")]
    {
        let _ = mime;
        // Driving the app over rinch's debug port cannot work a dialog: this
        // names a directory to save into instead. Never set in ordinary use.
        let path = match std::env::var("PIMBLE_DEBUG_SAVE_DIR") {
            Ok(dir) => Some(std::path::PathBuf::from(dir).join(name)),
            Err(_) => rinch::dialogs::save_file()
                .set_title("Save Picture As")
                .set_file_name(name)
                .add_filter(extension.to_ascii_uppercase(), &[extension])
                .save(),
        };
        let Some(path) = path else { return };
        match std::fs::write(&path, &bytes) {
            Ok(()) => crate::events::show_notice(store, format!("Saved \"{}\".", path.display())),
            Err(e) => crate::events::show_notice(store, format!("\"{}\" could not be written: {e}.", path.display())),
        }
    }
    #[cfg(not(feature = "native"))]
    {
        let _ = extension;
        browser::download(store, name, mime, &bytes);
    }
}

#[cfg(not(feature = "native"))]
mod browser {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;

    use super::AppStore;

    fn blob(mime: &str, bytes: &[u8]) -> Option<web_sys::Blob> {
        let array = js_sys::Uint8Array::from(bytes);
        let parts = js_sys::Array::of1(&array);
        let options = web_sys::BlobPropertyBag::new();
        options.set_type(mime);
        web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).ok()
    }

    /// A download of `bytes` named `name` (an anchor with `download`).
    pub(super) fn download(store: AppStore, name: &str, mime: &str, bytes: &[u8]) {
        let done = (|| {
            let blob = blob(mime, bytes)?;
            let url = web_sys::Url::create_object_url_with_blob(&blob).ok()?;
            let document = web_sys::window()?.document()?;
            let anchor = document.create_element("a").ok()?.dyn_into::<web_sys::HtmlAnchorElement>().ok()?;
            anchor.set_href(&url);
            anchor.set_download(name);
            anchor.click();
            let _ = web_sys::Url::revoke_object_url(&url);
            Some(())
        })();
        if done.is_none() {
            crate::events::show_notice(store, "The picture could not be saved.".to_string());
        }
    }

    /// A PNG on the clipboard (`navigator.clipboard.write` with a
    /// `ClipboardItem`, looked up at run time: not every browser has it).
    pub(super) fn copy_png(store: AppStore, png: Vec<u8>) {
        let written = (|| -> Option<js_sys::Promise> {
            let window = web_sys::window()?;
            let blob = blob("image/png", &png)?;
            let ctor = js_sys::Reflect::get(&window, &JsValue::from_str("ClipboardItem")).ok()?.dyn_into::<js_sys::Function>().ok()?;
            let items = js_sys::Object::new();
            js_sys::Reflect::set(&items, &JsValue::from_str("image/png"), &blob).ok()?;
            let item = js_sys::Reflect::construct(&ctor, &js_sys::Array::of1(&items)).ok()?;
            let clipboard = js_sys::Reflect::get(&window.navigator(), &JsValue::from_str("clipboard")).ok()?;
            let write = js_sys::Reflect::get(&clipboard, &JsValue::from_str("write")).ok()?.dyn_into::<js_sys::Function>().ok()?;
            write.call1(&clipboard, &js_sys::Array::of1(&item)).ok()?.dyn_into::<js_sys::Promise>().ok()
        })();
        match written {
            Some(promise) => wasm_bindgen_futures::spawn_local(async move {
                match wasm_bindgen_futures::JsFuture::from(promise).await {
                    Ok(_) => crate::events::show_notice(store, "The picture is on the clipboard.".to_string()),
                    Err(_) => crate::events::show_notice(store, "The browser did not let the picture onto the clipboard.".to_string()),
                }
            }),
            None => crate::events::show_notice(store, "This browser cannot put pictures on the clipboard.".to_string()),
        }
    }
}
