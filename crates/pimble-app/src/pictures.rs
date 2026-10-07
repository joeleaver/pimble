//! Pictures in a note's text (docs/IMAGES_CONTRACT.md "Showing and inserting
//! images"): adding one, and showing the ones a document names.
//!
//! A picture in a document is rinch's `image` node whose `src` is a
//! `pimble-blob:` URL; its bytes live once, beside the store, and reach this
//! app through the server and nothing else. So there are two halves here:
//!
//! * **Adding.** A paste, a drop and Edit > "Insert Image..." all end in
//!   [`add_picture`]: the bytes go to the backend (`PutBlob`: fitted under
//!   the size limit, then stored), and when the URL comes back
//!   ([`stored`]) the image is inserted where the picture was aimed, as an
//!   ordinary local edit. Nothing but that URL ever enters a document.
//! * **Showing.** rinch asks [`load`] for every `pimble-blob:` source. It
//!   answers from a small cache; a picture it does not hold is asked of the
//!   backend (`GetBlob`) and, when its bytes arrive ([`loaded`]), rinch is
//!   told to ask again. The same loader serves the desktop and the browser:
//!   rinch-web wraps the bytes in an object URL itself.
//!
//! [`PicturesPlugin`] is the rule for pasted HTML: an `<img>` that is not a
//! picture of an open store is left out, so no `data:` or remote address
//! enters a document by the back door.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;
use pimble_core::{BlobUrl, NodeId, StoreId};
use rinch::image::ImageLoadResult;
use rinch_editor_core::{Fragment, Node, PasteContent, Plugin, PluginKey, Slice};

use crate::panes::PaneId;
use crate::protocol::{BackendCommand, StoredBlob};
use crate::rinch_editor::SelectionAnchor;
use crate::state::AppStore;

/// The scheme of a picture's `src`, without the colon, as rinch registers it.
const SCHEME: &str = "pimble-blob";

/// What a picture command answers with no server to ask.
pub(crate) const NOT_CONNECTED: &str = "Pimble is not connected to its server, so the picture could not be reached.";

/// What the browser answers for a picture in an encrypted store: pictures in
/// ciphertext are a later wave (docs/IMAGES_CONTRACT.md, wave 3), and until
/// then nothing is written.
pub const ENCRYPTED_IN_BROWSER: &str = "Pictures in encrypted stores are not available in the browser yet.";

/// "Insert Image..." with no note in the focused pane.
const NO_NOTE: &str = "Open a note to add a picture to it.";

/// The picture was stored but its note had been closed meanwhile.
const NOTE_CLOSED: &str = "The note was closed before the picture was ready, so it was not added.";

/// The picture was stored but the caret is where no picture can go.
const NO_PLACE: &str = "A picture cannot go where the caret is.";

/// What pasted HTML lost to [`PicturesPlugin`].
const PASTE_STRIPPED: &str = "Pictures from other places were left out of what you pasted. Save a picture and add it with Edit > Insert Image.";

/// The file types "Insert Image..." offers, as extensions and as the
/// browser's `accept` list. The bytes decide in the end, not the name.
#[cfg(feature = "native")]
const EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "avif"];
#[cfg(not(feature = "native"))]
const ACCEPT: &str = "image/png,image/jpeg,image/gif,image/webp,image/avif";

// ── Adding ──────────────────────────────────────────────────────────────────

/// A picture on its way to the store, and where it goes when it is there.
struct Pending {
    pane: PaneId,
    store_id: StoreId,
    node_id: NodeId,
    anchor: SelectionAnchor,
    alt: String,
}

thread_local! {
    static PENDING: RefCell<HashMap<u64, Pending>> = RefCell::new(HashMap::new());
    static NEXT_REQUEST: Cell<u64> = const { Cell::new(1) };
}

/// The `alt` a picture goes in with: its file's name without the extension,
/// and nothing for a pasted bitmap, which has no name (the browser calls
/// every one of those `image.png`).
fn alt_from_name(name: Option<&str>, pasted: bool) -> String {
    let Some(name) = name else { return String::new() };
    if pasted && name.eq_ignore_ascii_case("image.png") {
        return String::new();
    }
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => name.to_string(),
    }
}

/// Add a picture to the note in pane `pane`, at `anchor`: the one way in,
/// whichever of the three ways the person chose. Answers at once; the image
/// appears when its bytes are stored ([`stored`]). A document this device
/// may only read takes none, and says so.
pub(crate) fn add_picture(store: AppStore, pane: PaneId, bytes: Vec<u8>, alt: String, anchor: SelectionAnchor) {
    let Some(active) = rinch::prelude::untracked(|| store.pane(pane).active_edit.get()) else {
        crate::events::show_notice(store, NO_NOTE.to_string());
        return;
    };
    if !store.node_access(active.store_id, active.node_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    let request_id = NEXT_REQUEST.with(|next| next.replace(next.get() + 1));
    PENDING.with(|pending| {
        pending
            .borrow_mut()
            .insert(request_id, Pending { pane, store_id: active.store_id, node_id: active.node_id, anchor, alt })
    });
    store.send(BackendCommand::PutBlob { store_id: active.store_id, node_id: active.node_id, bytes, request_id });
}

/// The answer to `PutBlob`: insert the image where it was aimed, and say what
/// there is to say (the picture was scaled down; it was refused).
pub(crate) fn stored(store: AppStore, request_id: u64, result: &Result<StoredBlob, String>) {
    let Some(pending) = PENDING.with(|pending| pending.borrow_mut().remove(&request_id)) else { return };
    let blob = match result {
        Ok(blob) => blob,
        Err(sentence) => {
            crate::events::show_notice(store, sentence.clone());
            return;
        }
    };
    let holds = rinch::prelude::untracked(|| store.pane(pending.pane).active_edit.get())
        .is_some_and(|edit| (edit.store_id, edit.node_id) == (pending.store_id, pending.node_id));
    if !holds {
        crate::events::show_notice(store, NOTE_CLOSED.to_string());
        return;
    }
    let handle = crate::editor::editor(pending.pane);
    let src = blob.url.to_string();
    // The anchor names the place the picture was aimed at however much was
    // typed meanwhile. It is lost when a peer's change re-projected the
    // document while the picture was on its way; the caret is then the best
    // place there is.
    let inserted = handle.insert_image_at(&pending.anchor, &src, &pending.alt) || handle.insert_image(&src, &pending.alt);
    if !inserted {
        crate::events::show_notice(store, NO_PLACE.to_string());
    } else if let Some(notice) = &blob.notice {
        crate::events::show_notice(store, notice.clone());
    }
}

/// Take pictures pasted into or dropped on pane `pane`'s editor
/// (`EditorHandle::on_image_input`). With this registered rinch never makes
/// a `data:` URL: the answer is always "later", and [`stored`] inserts.
pub(crate) fn take_image_input(store: AppStore, pane: PaneId) {
    use crate::rinch_editor::ImageInputSource;
    crate::editor::editor(pane).on_image_input(move |input| {
        let alt = alt_from_name(input.name.as_deref(), input.source == ImageInputSource::Paste);
        add_picture(store, pane, input.bytes, alt, input.anchor);
        None
    });
}

/// Edit > "Insert Image...": pick a picture and add it at the focused pane's
/// caret. A native menu item cannot be greyed out reactively, so a pane with
/// no note, or a note this device may only read, answers with a sentence.
pub fn insert_image(store: AppStore) {
    let pane = store.focused();
    let Some(active) = rinch::prelude::untracked(|| store.pane(pane).active_edit.get()) else {
        crate::events::show_notice(store, NO_NOTE.to_string());
        return;
    };
    if !store.node_access(active.store_id, active.node_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    // Taken before the dialog opens: the caret is the person's choice of
    // place, and a dialog may take the focus away.
    let anchor = crate::editor::editor(pane).anchor_selection();
    pick(store, pane, anchor);
}

/// The desktop's picker: rinch's file dialog, then the file's bytes.
#[cfg(feature = "native")]
fn pick(store: AppStore, pane: PaneId, anchor: SelectionAnchor) {
    // Driving the app over rinch's debug port (scripts/perf/README.md)
    // cannot work a native file dialog, paste from the clipboard or drop a
    // file. This names a file to take instead, and how it should arrive:
    // "paste" and "drop" hand it to the editor exactly as rinch does a
    // pasted or dropped picture. Never set in ordinary use.
    if let Ok(path) = std::env::var("PIMBLE_DEBUG_PICTURE") {
        let path = std::path::PathBuf::from(path);
        let name = path.file_name().map(|n| n.to_string_lossy().to_string());
        let Some(bytes) = read_picture(store, &path) else { return };
        let source = match std::env::var("PIMBLE_DEBUG_PICTURE_AS").as_deref() {
            Ok("paste") => Some(crate::rinch_editor::ImageInputSource::Paste),
            Ok("drop") => Some(crate::rinch_editor::ImageInputSource::Drop),
            _ => None,
        };
        match source {
            Some(source) => {
                let mime = pimble_core::ImageMime::sniff(&bytes).map(|m| m.as_str()).unwrap_or("application/octet-stream");
                let name = name.filter(|_| source == crate::rinch_editor::ImageInputSource::Drop);
                crate::editor::editor(pane).offer_image_input(source, bytes, mime, name);
            }
            None => add_picture(store, pane, bytes, alt_from_name(name.as_deref(), false), anchor),
        }
        return;
    }
    let Some(path) = rinch::dialogs::open_file().set_title("Insert Image").add_filter("Pictures", &EXTENSIONS).pick_file() else {
        return;
    };
    let name = path.file_name().map(|n| n.to_string_lossy().to_string());
    let Some(bytes) = read_picture(store, &path) else { return };
    add_picture(store, pane, bytes, alt_from_name(name.as_deref(), false), anchor);
}

#[cfg(feature = "native")]
fn read_picture(store: AppStore, path: &std::path::Path) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            crate::events::show_notice(store, format!("\"{}\" could not be read: {e}.", path.display()));
            None
        }
    }
}

/// The browser's picker: a hidden file input, clicked for the person. One at
/// a time; an input left behind by a cancelled dialog is replaced.
#[cfg(not(feature = "native"))]
fn pick(store: AppStore, pane: PaneId, anchor: SelectionAnchor) {
    use wasm_bindgen::JsCast;

    const INPUT_ID: &str = "pimble-picture-input";
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    if let Some(old) = document.get_element_by_id(INPUT_ID) {
        old.remove();
    }
    let Some(input) = document.create_element("input").ok().and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok()) else {
        return;
    };
    input.set_id(INPUT_ID);
    input.set_type("file");
    input.set_accept(ACCEPT);
    input.set_hidden(true);
    let Some(body) = document.body() else { return };
    if body.append_child(&input).is_err() {
        return;
    }
    let picked = input.clone();
    let on_change = wasm_bindgen::closure::Closure::once_into_js(move |_: web_sys::Event| {
        let file = picked.files().and_then(|files| files.get(0));
        picked.remove();
        let Some(file) = file else { return };
        wasm_bindgen_futures::spawn_local(async move {
            let name = file.name();
            match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => {
                    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                    add_picture(store, pane, bytes, alt_from_name(Some(&name), false), anchor);
                }
                Err(e) => {
                    tracing::warn!("reading {name} failed: {e:?}");
                    crate::events::show_notice(store, format!("\"{name}\" could not be read."));
                }
            }
        });
    });
    if input.add_event_listener_with_callback("change", on_change.unchecked_ref()).is_err() {
        return;
    }
    input.click();
}

// ── Showing ─────────────────────────────────────────────────────────────────

/// How many bytes of pictures the cache keeps before it lets the oldest go.
/// rinch keeps what it decoded; this only spares a second fetch when it asks
/// again (another pane, a reload).
const CACHE_BYTES: usize = 64 * 1024 * 1024;

/// What [`load`] knows, shared with the thread rinch calls it on.
#[derive(Default)]
struct Shown {
    /// Pictures fetched, oldest first in `order`.
    held: HashMap<BlobUrl, Arc<Vec<u8>>>,
    order: VecDeque<BlobUrl>,
    held_bytes: usize,
    /// Asked of the backend and not answered yet, with the `src` each was
    /// asked under (rinch reloads by the string the document spelled).
    asked: HashMap<BlobUrl, HashSet<String>>,
    /// Answered "not here": not asked again until [`retry_missing`].
    missing: HashMap<BlobUrl, HashSet<String>>,
    /// Where `GetBlob` goes. The loader runs on a thread of rinch's own on
    /// the desktop, where the app's state cannot be reached.
    backend: Option<Sender<BackendCommand>>,
}

#[cfg(not(test))]
static SHOWN: Mutex<Option<Shown>> = Mutex::new(None);

#[cfg(not(test))]
fn shown<T>(f: impl FnOnce(&mut Shown) -> T) -> T {
    let mut shown = SHOWN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(shown.get_or_insert_with(Default::default))
}

// Under `cargo test` each test is a thread with an app of its own, and one
// test's backend must not be asked for another's pictures.
#[cfg(test)]
thread_local! {
    static SHOWN: RefCell<Shown> = RefCell::new(Shown::default());
}

#[cfg(test)]
fn shown<T>(f: impl FnOnce(&mut Shown) -> T) -> T {
    SHOWN.with(|shown| f(&mut shown.borrow_mut()))
}

/// A document is opening in pane `pane`: answer for `pimble-blob:` sources
/// (registered once per process), tell the loader which backend to ask (it
/// runs on a thread of rinch's own on the desktop), take the pictures pasted
/// into or dropped on the pane's editor, and ask again for any picture that
/// was not here. All of it before the document's content, so before any
/// picture of it can be asked for.
pub(crate) fn opening(store: AppStore, pane: PaneId) {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| rinch::image::register_image_scheme(SCHEME, load));
    connect(store);
    take_image_input(store, pane);
    retry_missing();
}

/// Point the loader at the backend the app talks to now.
fn connect(store: AppStore) {
    let backend = rinch::prelude::untracked(|| store.backend.with(|b| b.as_ref().map(|b| b.cmd_tx.clone())));
    shown(|shown| shown.backend = backend);
}

/// rinch's loader for `pimble-blob:` sources: the picture's encoded bytes if
/// they are here, else "not yet" and a request for them. Never blocks, and
/// never reads a store's files: the server is the one way in.
fn load(src: &str) -> ImageLoadResult {
    let Some(url) = BlobUrl::parse(src) else {
        return ImageLoadResult::Failed(format!("{src} is not a picture's address"));
    };
    shown(|shown| {
        if let Some(bytes) = shown.held.get(&url) {
            return ImageLoadResult::Loaded(bytes.to_vec());
        }
        if let Some(sources) = shown.missing.get_mut(&url) {
            sources.insert(src.to_string());
        } else {
            let first = !shown.asked.contains_key(&url);
            shown.asked.entry(url).or_default().insert(src.to_string());
            if first {
                if let Some(backend) = &shown.backend {
                    let _ = backend.try_send(BackendCommand::GetBlob { url });
                }
            }
        }
        ImageLoadResult::Failed("This picture has not arrived yet.".to_string())
    })
}

/// The answer to `GetBlob`: keep the bytes and have rinch ask again, or
/// remember that the picture is not here.
pub(crate) fn loaded(url: BlobUrl, result: &Result<Arc<Vec<u8>>, String>) {
    let sources = shown(|shown| {
        let sources = shown.asked.remove(&url).unwrap_or_default();
        match result {
            Ok(bytes) => {
                if shown.held.insert(url, bytes.clone()).is_none() {
                    shown.order.push_back(url);
                    shown.held_bytes += bytes.len();
                }
                // The newest is kept whatever its size.
                while shown.held_bytes > CACHE_BYTES && shown.order.len() > 1 {
                    if let Some(oldest) = shown.order.pop_front() {
                        if let Some(dropped) = shown.held.remove(&oldest) {
                            shown.held_bytes -= dropped.len();
                        }
                    }
                }
                sources
            }
            Err(reason) => {
                tracing::info!("picture {url} is not here: {reason}");
                shown.missing.entry(url).or_default().extend(sources);
                HashSet::new()
            }
        }
    });
    for src in sources {
        rinch::image::reload_image(&src);
    }
}

/// Ask again for every picture that was not here: it may have arrived since.
/// Called when a document opens and when the connection comes back.
pub(crate) fn retry_missing() {
    let sources: Vec<String> = shown(|shown| shown.missing.drain().flat_map(|(_, sources)| sources).collect());
    for src in sources {
        rinch::image::reload_image(&src);
    }
}

// ── Pasted HTML ─────────────────────────────────────────────────────────────

/// The paste rule for pictures: pasted HTML keeps an `<img>` only when its
/// `src` is a `pimble-blob:` URL of a store that is open here. Anything
/// else (a `data:` URL, a web address, a file path) is left out and the rest
/// of the paste goes in as it would have. Fetching a remote picture into a
/// blob is not done.
pub(crate) struct PicturesPlugin;

impl Plugin for PicturesPlugin {
    fn key(&self) -> PluginKey {
        PluginKey("pimble.pictures")
    }

    fn handle_paste(&self, state: &rinch_editor_core::state::EditorState, paste: &PasteContent) -> Option<rinch_editor_core::state::Transaction> {
        let html = paste.html.as_deref()?;
        if !html.to_ascii_lowercase().contains("<img") {
            return None;
        }
        let slice = rinch_editor_core::serialize::html::slice_from_html(state.schema(), html).ok()?;
        let kept = strip_foreign_images(&slice, &|src| {
            BlobUrl::parse(src).is_some_and(|url| crate::links::with_app_store(|store| store.get_store_signal(url.store).is_some()).unwrap_or(false))
        })?;
        crate::links::later(|store| crate::events::show_notice(store, PASTE_STRIPPED.to_string()));
        // In code a paste is its text, and the default does that.
        let in_code = state.doc.resolve(state.selection.from()).is_ok_and(|pos| pos.parent().node_type().spec().code);
        if in_code && paste.text.is_some() {
            return None;
        }
        let mut tr = state.tr();
        if kept.content.child_count() > 0 && tr.replace_selection(kept).is_err() {
            // Nowhere to put what is left (a selection of table cells):
            // nothing is pasted rather than the pictures let through.
            return Some(state.tr());
        }
        Some(tr)
    }
}

/// `slice` without the images `keep` does not vouch for, or `None` when it
/// holds none of those (the paste is then nobody's business here). A block
/// at the top of the slice that held nothing but such pictures goes with
/// them, unless it is an open edge of the slice.
fn strip_foreign_images(slice: &Slice, keep: &dyn Fn(&str) -> bool) -> Option<Slice> {
    fn strip(node: &Node, keep: &dyn Fn(&str) -> bool, removed: &mut bool) -> Option<Node> {
        if node.type_name() == "image" {
            if keep(node.attrs().get_str("src").unwrap_or_default()) {
                return Some(node.clone());
            }
            *removed = true;
            return None;
        }
        if node.is_text() || node.child_count() == 0 {
            return Some(node.clone());
        }
        let children: Vec<Node> = node.content().iter().filter_map(|child| strip(child, keep, removed)).collect();
        Some(node.copy_with_content(Fragment::from_children(children)))
    }

    let mut removed = false;
    let count = slice.content.child_count();
    let mut top = Vec::with_capacity(count);
    for (i, child) in slice.content.iter().enumerate() {
        let Some(stripped) = strip(child, keep, &mut removed) else { continue };
        let open_edge = (i == 0 && slice.open_start > 0) || (i + 1 == count && slice.open_end > 0);
        let emptied = child.is_textblock() && child.child_count() > 0 && stripped.child_count() == 0;
        if emptied && !open_edge {
            continue;
        }
        top.push(stripped);
    }
    removed.then(|| Slice::new(Fragment::from_children(top), slice.open_start, slice.open_end))
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::protocol::{BackendEvent, BackendHandle};
    use crossbeam_channel::bounded;

    const P: [PaneId; crate::panes::MAX_PANES] = PaneId::ALL;

    fn store_with_commands() -> (AppStore, crossbeam_channel::Receiver<BackendCommand>) {
        let store = AppStore::new();
        let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(256);
        let (_event_tx, event_rx) = bounded::<BackendEvent>(1);
        store.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
        (store, cmd_rx)
    }

    fn images(node: &Node, out: &mut Vec<(String, String)>) {
        if node.type_name() == "image" {
            let attr = |name| node.attrs().get_str(name).unwrap_or_default().to_string();
            out.push((attr("src"), attr("alt")));
        }
        for child in node.content().iter() {
            images(child, out);
        }
    }

    fn images_in(pane: PaneId) -> Vec<(String, String)> {
        let mut out = Vec::new();
        images(&crate::editor::editor(pane).doc(), &mut out);
        out
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-not-really-a-picture";

    #[test]
    fn a_name_becomes_alt_text_and_a_pasted_bitmap_has_none() {
        assert_eq!(alt_from_name(Some("holiday.jpg"), false), "holiday");
        assert_eq!(alt_from_name(Some("/home/a/two.words.png"), false), "two.words");
        assert_eq!(alt_from_name(Some("noextension"), false), "noextension");
        assert_eq!(alt_from_name(Some(".hidden"), false), ".hidden");
        assert_eq!(alt_from_name(None, true), "");
        assert_eq!(alt_from_name(Some("image.png"), true), "", "the browser's name for a clipboard bitmap");
        assert_eq!(alt_from_name(Some("image.png"), false), "image");
        assert_eq!(alt_from_name(Some("cat.png"), true), "cat", "a file copied in a file manager keeps its name");
    }

    /// The three ways in are one: a pasted picture is sent to be stored and
    /// goes into the document, where it was aimed, when its URL comes back;
    /// a refusal inserts nothing and is said; a read-only note takes none.
    #[test]
    fn a_picture_is_stored_first_and_inserted_when_its_url_comes_back() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, P[0], store_id, node_id, &[]);
        let handle = crate::editor::editor(P[0]);
        assert!(handle.insert_text("before after"));
        handle.set_selection(rinch_editor_core::Selection::cursor(rinch_editor_core::Pos(8)));
        let _ = commands.try_iter().count();

        use crate::rinch_editor::ImageInputSource;
        assert!(!handle.offer_image_input(ImageInputSource::Drop, PNG.to_vec(), "image/png", Some("cat.png".to_string())), "the answer is later");
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        let request_id = match sent.as_slice() {
            [BackendCommand::PutBlob { store_id: s, node_id: n, bytes, request_id }] => {
                assert_eq!((*s, *n, bytes.as_slice()), (store_id, node_id, PNG));
                *request_id
            }
            other => panic!("unexpected {other:?}"),
        };
        assert!(images_in(P[0]).is_empty(), "nothing enters the document until the picture is stored");

        // Typed meanwhile, before the place the picture was aimed at.
        handle.set_selection(rinch_editor_core::Selection::cursor(rinch_editor_core::Pos(1)));
        assert!(handle.insert_text("typed "));

        let url = BlobUrl::new(store_id, pimble_core::BlobId::new());
        stored(store, request_id, &Ok(StoredBlob { url, notice: Some("It was scaled.".to_string()) }));
        assert_eq!(images_in(P[0]), vec![(url.to_string(), "cat".to_string())]);
        let doc = handle.doc();
        let paragraph = doc.child(0);
        assert_eq!(paragraph.child(0).text(), Some("typed before "), "where it was aimed, not where the caret went");
        assert_eq!(paragraph.child(1).type_name(), "image");
        assert_eq!(store.notice.get(), "It was scaled.");
        assert!(
            commands.try_iter().any(|c| matches!(c, BackendCommand::BroadcastChanges { node_id: n, .. } if n == node_id)),
            "the insert is an ordinary edit and travels like typing"
        );

        // An answer nobody is waiting for does nothing.
        stored(store, request_id, &Ok(StoredBlob { url, notice: None }));
        assert_eq!(images_in(P[0]).len(), 1);

        // A refusal: its sentence, and nothing inserted.
        assert!(!handle.offer_image_input(ImageInputSource::Paste, b"<svg/>".to_vec(), "image/svg+xml", None));
        let request_id = commands.try_iter().find_map(|c| match c {
            BackendCommand::PutBlob { request_id, .. } => Some(request_id),
            _ => None,
        });
        stored(store, request_id.expect("asked"), &Err(pimble_image::FitError::NotAnImage.to_string()));
        assert_eq!(images_in(P[0]).len(), 1);
        assert_eq!(store.notice.get(), pimble_image::FitError::NotAnImage.to_string());

        // The note closed while a picture was on its way: stored, not added.
        assert!(!handle.offer_image_input(ImageInputSource::Paste, PNG.to_vec(), "image/png", None));
        let request_id = commands.try_iter().find_map(|c| match c {
            BackendCommand::PutBlob { request_id, .. } => Some(request_id),
            _ => None,
        });
        crate::editor::stop_editing(store, P[0]);
        stored(store, request_id.expect("asked"), &Ok(StoredBlob { url, notice: None }));
        assert_eq!(store.notice.get(), NOTE_CLOSED);

        // No note in the pane: the menu says so and asks for nothing.
        let _ = commands.try_iter().count();
        store.focus_pane(P[0]);
        insert_image(store);
        assert_eq!(store.notice.get(), NO_NOTE);
        assert!(commands.try_iter().all(|c| !matches!(c, BackendCommand::PutBlob { .. })));
    }

    /// A read-only editor is offered nothing by rinch, and `add_picture`
    /// itself refuses a note this device may only read.
    #[test]
    fn a_read_only_note_takes_no_picture() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, P[1], store_id, node_id, &[]);
        let handle = crate::editor::editor(P[1]);
        let _ = commands.try_iter().count();

        crate::editor::set_read_only(P[1], true);
        use crate::rinch_editor::ImageInputSource;
        assert!(!handle.offer_image_input(ImageInputSource::Paste, PNG.to_vec(), "image/png", None));
        assert!(commands.try_iter().all(|c| !matches!(c, BackendCommand::PutBlob { .. })), "a locked editor is offered nothing");
        crate::editor::set_read_only(P[1], false);

        // The server's word on the node, which is what the menu and
        // `add_picture` go by.
        let mut node = pimble_core::Node::new("document");
        node.id = node_id;
        node.access = pimble_core::StoreAccess::Read;
        store.node_data.update(|map| {
            map.insert((store_id, node_id), rinch::prelude::Signal::new(node));
        });
        add_picture(store, P[1], PNG.to_vec(), String::new(), handle.anchor_selection());
        store.focus_pane(P[1]);
        insert_image(store);
        assert!(commands.try_iter().all(|c| !matches!(c, BackendCommand::PutBlob { .. })));
        assert_eq!(store.notice.get(), pimble_core::StoreAccess::READ_ONLY_REFUSAL);
        assert!(images_in(P[1]).is_empty());
        crate::editor::stop_editing(store, P[1]);
    }

    /// The loader: a picture it does not hold is asked for once and answered
    /// "not yet"; when the bytes come it has them; one that is not there is
    /// not asked for again until something says it may have arrived.
    #[test]
    fn the_loader_asks_once_and_answers_from_what_arrived() {
        let (store, commands) = store_with_commands();
        connect(store);
        let asked = |commands: &crossbeam_channel::Receiver<BackendCommand>, url: BlobUrl| {
            commands.try_iter().filter(|c| matches!(c, BackendCommand::GetBlob { url: u } if *u == url)).count()
        };

        let url = BlobUrl::new(StoreId::new(), pimble_core::BlobId::new());
        let src = url.to_string();
        assert!(matches!(load(&src), ImageLoadResult::Failed(_)));
        assert!(matches!(load(&src), ImageLoadResult::Failed(_)));
        assert_eq!(asked(&commands, url), 1, "once, however many elements name it");

        loaded(url, &Ok(Arc::new(PNG.to_vec())));
        match load(&src) {
            ImageLoadResult::Loaded(bytes) => assert_eq!(bytes, PNG),
            ImageLoadResult::Failed(e) => panic!("{e}"),
        }
        assert_eq!(asked(&commands, url), 0);

        let absent = BlobUrl::new(StoreId::new(), pimble_core::BlobId::new());
        let absent_src = absent.to_string();
        assert!(matches!(load(&absent_src), ImageLoadResult::Failed(_)));
        connect(store);
        assert_eq!(asked(&commands, absent), 1);
        loaded(absent, &Err("Blob not found".to_string()));
        assert!(matches!(load(&absent_src), ImageLoadResult::Failed(_)));
        assert_eq!(asked(&commands, absent), 0, "a picture that is not here is not asked for on every look");
        retry_missing();
        assert!(matches!(load(&absent_src), ImageLoadResult::Failed(_)));
        assert_eq!(asked(&commands, absent), 1, "until it may have arrived");

        assert!(matches!(load("pimble-blob:nonsense"), ImageLoadResult::Failed(_)));
        assert!(matches!(load("https://example.com/a.png"), ImageLoadResult::Failed(_)));
    }

    /// Pasted HTML keeps a picture of an open store and loses every other
    /// `<img>`; the words around them are pasted as ever.
    #[test]
    fn pasted_html_keeps_only_pictures_of_an_open_store() {
        let store = AppStore::new();
        crate::links::set_app_store(store);
        let open = pimble_core::Store::new_local("Notes", std::path::PathBuf::from("/nowhere/notes.pimble"));
        let ours = BlobUrl::new(open.id, pimble_core::BlobId::new());
        let elsewhere = BlobUrl::new(StoreId::new(), pimble_core::BlobId::new());
        store.store_data.update(|map| {
            map.insert(open.id, rinch::prelude::Signal::new(open));
        });

        let handle = crate::rinch_editor::create_editor();
        handle.add_plugin(std::rc::Rc::new(PicturesPlugin));
        let html = format!(
            "<p>one <img src=\"data:image/png;base64,AAAA\" alt=\"inline\"> two</p>\
             <p><img src=\"https://example.com/a.png\"></p>\
             <p><img src=\"{elsewhere}\"><img src=\"{ours}\" alt=\"kept\"> three</p>"
        );
        assert!(handle.paste(&PasteContent::new(Some("one two three".to_string()), Some(html))));
        let mut found = Vec::new();
        images(&handle.doc(), &mut found);
        assert_eq!(found, vec![(ours.to_string(), "kept".to_string())]);
        let doc = handle.doc();
        let text: Vec<String> = (0..doc.child_count())
            .map(|i| {
                let block = doc.child(i);
                (0..block.child_count()).filter_map(|j| block.child(j).text().map(str::to_string)).collect::<String>()
            })
            .collect();
        assert_eq!(text, vec!["one  two".to_string(), " three".to_string()], "the block that held only a foreign picture is gone");

        // Only foreign pictures: nothing is pasted, and no `data:` URL slips
        // in through the default path.
        let handle = crate::rinch_editor::create_editor();
        handle.add_plugin(std::rc::Rc::new(PicturesPlugin));
        handle.paste(&PasteContent::new(None, Some("<img src=\"data:image/png;base64,AAAA\">".to_string())));
        let mut found = Vec::new();
        images(&handle.doc(), &mut found);
        assert!(found.is_empty());

        // No pictures at all: not this plugin's paste.
        let handle = crate::rinch_editor::create_editor();
        handle.add_plugin(std::rc::Rc::new(PicturesPlugin));
        assert!(handle.paste(&PasteContent::new(None, Some("<p>plain <b>words</b></p>".to_string()))));
        assert_eq!(handle.doc().child(0).child_count(), 2);
    }
}
