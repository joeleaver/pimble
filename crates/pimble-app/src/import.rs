//! File > Import (docs/IMPORT_CONTRACT.md): RTF, Word and Scrivener, on the
//! desktop and in the browser.
//!
//! Three steps. A picker reads what the person chose ([`pick`]: a native
//! dialog on the desktop, a hidden file input in the browser; a Scrivener
//! project is a directory on both, and only the files it needs are read). The
//! import modal then asks where it goes: under the selected node, or into a
//! new store made for it ([`under_selection`], [`into_new_store`]). Last, the
//! backend parses the files with `pimble-import` and [`write`] makes the nodes
//! with the same commands the app writes with everywhere else (`CreateNode`,
//! `GetNode`, `SetNodeContent`), so an import is an ordinary edit: it works
//! in a plain store, an encrypted one, a share and a replica alike, and it
//! reaches every other device the way typing does.

use std::cell::RefCell;

use pimble_core::{custom_keys, node_types, NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_import::{Files, Format, Imported};
use rinch::prelude::untracked;

use crate::protocol::{BackendCommand, BackendEvent};
use crate::state::{parse_tree_value, AppStore};

/// What was picked, waiting for the modal to say where it goes.
struct Picked {
    format: Format,
    name: String,
    files: Files,
}

thread_local! {
    /// The pick the modal is open for.
    static PICKED: RefCell<Option<Picked>> = const { RefCell::new(None) };
    /// A pick waiting for the store made for it to open: `None` until the
    /// store's id is known (`StoreCreated`, `HostedStoreCreated`).
    static AWAITING_STORE: RefCell<Option<(Picked, Option<StoreId>)>> = const { RefCell::new(None) };
}

/// The menu's three items, in order.
pub const FORMATS: [Format; 3] = [Format::Rtf, Format::Docx, Format::Scrivener];

/// The menu label for a format.
pub fn menu_label(format: Format) -> &'static str {
    match format {
        Format::Rtf => "Import RTF...",
        Format::Docx => "Import Word Document...",
        Format::Scrivener => "Import Scrivener Project...",
    }
}

/// What the modal calls a pick.
fn describe(format: Format, name: &str) -> String {
    match format {
        Format::Rtf => format!("{name}, an RTF file"),
        Format::Docx => format!("{name}, a Word document"),
        Format::Scrivener => format!("{name}, a Scrivener project"),
    }
}

/// File > Import > a format: pick, then ask where.
pub fn start(store: AppStore, format: Format) {
    pick(store, format);
}

/// The picker has read what was chosen: open the modal for it.
fn picked(store: AppStore, format: Format, name: String, files: Files) {
    if files.is_empty() {
        crate::events::show_notice(store, format!("Nothing in \"{name}\" can be imported as {}.", format.label()));
        return;
    }
    store.import_modal_label.set(describe(format, &name));
    store.import_modal_store_name.set(pimble_import::title_from_name(&name));
    store.import_modal_under.set(selection_target(store));
    store.import_modal_error.set(String::new());
    PICKED.with(|p| *p.borrow_mut() = Some(Picked { format, name, files }));
    store.import_modal_open.set(true);
}

/// Where "under the selected node" would put it: the selected row's node, a
/// mount's source rather than the mount, a store row's root. `None` when
/// nothing is selected or this device may only read it.
fn selection_target(store: AppStore) -> Option<(StoreId, NodeId, String)> {
    let (store_id, node_id) = parse_tree_value(&untracked(|| store.selected_id.get())?)?;
    let node_id = node_id.or_else(|| store.root_node_id(store_id))?;
    let (store_id, node_id, title) = match store.get_node_signal(store_id, node_id).map(|s| untracked(|| s.get())) {
        Some(node) => match node.mount_ref() {
            Some(mount) => (mount.source_store, mount.source_node, node.metadata.title.clone()),
            None => (store_id, node_id, node.metadata.title.clone()),
        },
        None => (store_id, node_id, String::new()),
    };
    if !store.node_access(store_id, node_id).allows_write() {
        return None;
    }
    let title = if title.trim().is_empty() {
        store.get_store_signal(store_id).map(|s| untracked(|| s.get().name)).unwrap_or_else(|| "Untitled".to_string())
    } else {
        title
    };
    Some((store_id, node_id, title))
}

/// The modal closed without importing.
pub fn cancel(store: AppStore) {
    PICKED.with(|p| p.borrow_mut().take());
    store.import_modal_open.set(false);
}

/// "Under <node>": send it.
pub fn under_selection(store: AppStore) {
    let Some((store_id, node_id, _)) = untracked(|| store.import_modal_under.get()) else { return };
    let Some(picked) = PICKED.with(|p| p.borrow_mut().take()) else { return };
    store.import_modal_open.set(false);
    send(store, picked, store_id, Some(node_id));
}

/// "Into a new store": make the store, and import into it once it is open
/// ([`store_created`], [`store_opened`]).
pub fn into_new_store(store: AppStore) {
    let name = untracked(|| store.import_modal_store_name.get()).trim().to_string();
    if name.is_empty() {
        store.import_modal_error.set("Give the store a name.".to_string());
        return;
    }
    if !make_store(store, &name) {
        return;
    }
    let Some(picked) = PICKED.with(|p| p.borrow_mut().take()) else { return };
    store.import_modal_open.set(false);
    crate::events::show_notice(store, format!("Making the store \"{name}\" to import into..."));
    AWAITING_STORE.with(|a| *a.borrow_mut() = Some((picked, None)));
}

/// The desktop makes a store as a directory the person names in a save
/// dialog, exactly as File > "New Store..." does. Answers whether it went.
#[cfg(feature = "native")]
fn make_store(store: AppStore, name: &str) -> bool {
    let Some(path) = rinch::dialogs::save_file()
        .set_title("Create Store for the Import")
        .set_file_name(format!("{name}.pimble"))
        .add_filter("Pimble Store", &["pimble"])
        .save()
    else {
        return false;
    };
    let path = path.to_string_lossy().to_string();
    let path = if path.ends_with(".pimble") { path } else { format!("{path}.pimble") };
    store.pending_create_path.set(Some(path.clone()));
    store.send(BackendCommand::CreateStore { path, name: name.to_string() });
    true
}

/// The browser makes an encrypted store on the account, as the explorer's
/// "+" does.
#[cfg(not(feature = "native"))]
fn make_store(store: AppStore, name: &str) -> bool {
    store.send(BackendCommand::CreateHostedStore { name: name.to_string(), kind: "vault".to_string() });
    true
}

/// A store was made. If an import is waiting for one, it is this one.
pub(crate) fn store_created(store_id: StoreId) {
    AWAITING_STORE.with(|a| {
        if let Some((_, target @ None)) = a.borrow_mut().as_mut() {
            *target = Some(store_id);
        }
    });
}

/// Making the store failed: the import waiting for it is dropped.
pub(crate) fn store_failed() -> bool {
    AWAITING_STORE.with(|a| {
        let mut a = a.borrow_mut();
        match a.as_ref() {
            Some((_, None)) => a.take().is_some(),
            _ => false,
        }
    })
}

/// A store is open in the app. If an import is waiting for it, send it, into
/// the store's root.
pub(crate) fn store_opened(store: AppStore, store_id: StoreId, root: Option<NodeId>) {
    let ready = AWAITING_STORE.with(|a| {
        let mut a = a.borrow_mut();
        match a.as_ref() {
            Some((_, Some(id))) if *id == store_id => a.take().map(|(picked, _)| picked),
            _ => None,
        }
    });
    if let Some(picked) = ready {
        send(store, picked, store_id, root);
    }
}

fn send(store: AppStore, picked: Picked, store_id: StoreId, parent_id: Option<NodeId>) {
    crate::events::show_notice(store, format!("Importing {}...", picked.name));
    store.send(BackendCommand::Import { store_id, parent_id, format: picked.format, name: picked.name, files: picked.files });
}

// ── Picking ─────────────────────────────────────────────────────────────────

/// The desktop's picker: rinch's file dialog for a file, its folder dialog
/// for a Scrivener project.
#[cfg(feature = "native")]
fn pick(store: AppStore, format: Format) {
    let title = format!("Import {}", format.label());
    // Driving the app over rinch's debug port (scripts/perf/README.md)
    // cannot work a native dialog: this names what to take instead. Never
    // set in ordinary use.
    let debug = std::env::var_os("PIMBLE_DEBUG_IMPORT").map(std::path::PathBuf::from);
    let path = match format {
        _ if debug.is_some() => debug,
        Format::Scrivener => rinch::dialogs::pick_folder().set_title(&title).pick(),
        _ => rinch::dialogs::open_file().set_title(&title).add_filter(format.label(), format.extensions()).pick_file(),
    };
    let Some(path) = path else { return };
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let read = match format {
        Format::Scrivener => pimble_import::read_dir(format, &path),
        _ => std::fs::read(&path).map(|bytes| Files::from([(name.clone(), bytes)])),
    };
    match read {
        Ok(files) => picked(store, format, name, files),
        Err(e) => crate::events::show_notice(store, format!("\"{}\" could not be read: {e}.", path.display())),
    }
}

/// The browser's picker: a hidden file input, a directory input for a
/// Scrivener project. Only the files the import reads are read.
#[cfg(not(feature = "native"))]
fn pick(store: AppStore, format: Format) {
    use wasm_bindgen::JsCast;

    const INPUT_ID: &str = "pimble-import-input";
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    if let Some(old) = document.get_element_by_id(INPUT_ID) {
        old.remove();
    }
    let Some(input) = document.create_element("input").ok().and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok()) else {
        return;
    };
    input.set_id(INPUT_ID);
    input.set_type("file");
    match format {
        Format::Scrivener => input.set_webkitdirectory(true),
        _ => input.set_accept(&format.extensions().iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(",")),
    }
    input.set_hidden(true);
    let Some(body) = document.body() else { return };
    if body.append_child(&input).is_err() {
        return;
    }
    let chosen = input.clone();
    let on_change = wasm_bindgen::closure::Closure::once_into_js(move |_: web_sys::Event| {
        let list = chosen.files();
        chosen.remove();
        let Some(list) = list else { return };
        let all: Vec<web_sys::File> = (0..list.length()).filter_map(|i| list.get(i)).collect();
        wasm_bindgen_futures::spawn_local(async move {
            // A directory's files are named by their path under what was
            // picked, the picked directory first ("Novel.scriv/Files/...").
            let mut name = String::new();
            let mut files = Files::new();
            for file in all {
                let (top, relative) = match format {
                    Format::Scrivener => {
                        // `webkitRelativePath`, which web-sys does not bind.
                        let path = js_sys::Reflect::get(&file, &"webkitRelativePath".into())
                            .ok()
                            .and_then(|v| v.as_string())
                            .unwrap_or_default();
                        match path.split_once('/') {
                            Some((top, rest)) => (top.to_string(), rest.to_string()),
                            None => continue,
                        }
                    }
                    _ => (file.name(), file.name()),
                };
                if !format.wants(&relative) {
                    continue;
                }
                name = top;
                match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                    Ok(buffer) => {
                        files.insert(relative, js_sys::Uint8Array::new(&buffer).to_vec());
                    }
                    Err(e) => {
                        tracing::warn!("reading {relative} failed: {e:?}");
                        crate::events::show_notice(store, format!("\"{relative}\" could not be read."));
                        return;
                    }
                }
            }
            picked(store, format, name, files);
        });
    });
    if input.add_event_listener_with_callback("change", on_change.unchecked_ref()).is_err() {
        return;
    }
    input.click();
}

// ── Writing ─────────────────────────────────────────────────────────────────

/// Whatever answers the app's commands: `process_command` on the desktop, the
/// browser's routing (the vault client for an encrypted store, the store's
/// server for a plain one).
#[allow(async_fn_in_trait)]
pub trait Runner {
    async fn run(&mut self, cmd: BackendCommand) -> Option<BackendEvent>;
}

/// Answer an `Import`: parse, write, and say how it went.
pub async fn import<R: Runner>(
    runner: &mut R,
    store_id: StoreId,
    parent_id: Option<NodeId>,
    format: Format,
    name: &str,
    files: &Files,
) -> BackendEvent {
    let tree = match pimble_import::import(format, name, files) {
        Ok(tree) => tree,
        Err(e) => return BackendEvent::ImportFailed { message: e.to_string() },
    };
    write(runner, store_id, parent_id, &tree).await
}

/// Make `tree` under `parent_id`, a node at a time, each one's children in
/// order. A failure part way says how far it got: what was made stays, as
/// it would after any other interrupted edit.
pub async fn write<R: Runner>(runner: &mut R, store_id: StoreId, parent_id: Option<NodeId>, tree: &Imported) -> BackendEvent {
    let total = tree.count();
    let failed = |made: usize, message: String| BackendEvent::ImportFailed {
        message: if made == 0 {
            format!("Nothing was imported: {message}")
        } else {
            format!("The import stopped after {made} of {total} notes: {message}")
        },
    };
    let top = match write_one(runner, store_id, parent_id, tree).await {
        Ok(id) => id,
        Err(message) => return failed(0, message),
    };
    let mut made = 1;
    let mut pending: Vec<(NodeId, &Imported)> = vec![(top, tree)];
    while let Some((id, node)) = pending.pop() {
        for child in &node.children {
            match write_one(runner, store_id, Some(id), child).await {
                Ok(child_id) => {
                    made += 1;
                    pending.push((child_id, child));
                }
                Err(message) => return failed(made, message),
            }
        }
    }
    BackendEvent::Imported { store_id, parent_id, node_id: top, title: tree.title.clone(), count: made }
}

/// One node: create it, then write its fields and content as one update of
/// the document the server made.
async fn write_one<R: Runner>(runner: &mut R, store_id: StoreId, parent_id: Option<NodeId>, item: &Imported) -> Result<NodeId, String> {
    let node_id = match runner.run(BackendCommand::CreateNode { store_id, parent_id, title: item.title.clone() }).await {
        Some(BackendEvent::NodeCreated { node_id, .. }) => node_id,
        other => return Err(unexpected(other)),
    };
    let content = match runner.run(BackendCommand::GetNode { store_id, node_id }).await {
        Some(BackendEvent::NodeLoaded { node, .. }) => node.content,
        other => return Err(unexpected(other)),
    };
    let delta = fill(&content, item).map_err(|e| format!("\"{}\" could not be written: {e}", item.title))?;
    match runner.run(BackendCommand::SetNodeContent { store_id, node_id, content: delta }).await {
        Some(BackendEvent::NodeContentUpdated { .. }) => Ok(node_id),
        other => Err(unexpected(other)),
    }
}

/// The update that gives a freshly created node's document `item`'s type,
/// appearance, tags and blocks. Its title came with `CreateNode`, and is
/// marked as chosen so the tree never retitles it from its first words.
fn fill(document: &[u8], item: &Imported) -> pimble_crdt::Result<Vec<u8>> {
    let mut doc = NodeDoc::load(document)?;
    let before = doc.state_vector();
    let now = chrono::Utc::now().to_rfc3339();
    if item.node_type != node_types::DOCUMENT {
        doc.set_node_type(item.node_type, &now)?;
    }
    doc.set_custom(custom_keys::EXPLICIT_TITLE, &serde_json::json!(true), &now)?;
    if let Some(icon) = &item.icon {
        doc.set_custom(custom_keys::ICON, &serde_json::json!(icon), &now)?;
    }
    if let Some(color) = &item.color {
        doc.set_custom(custom_keys::COLOR, &serde_json::json!(color), &now)?;
    }
    if let Some(background) = &item.background {
        doc.set_custom(custom_keys::BACKGROUND, &serde_json::json!(background), &now)?;
    }
    if !item.tags.is_empty() {
        doc.set_tags(&item.tags, &now)?;
    }
    if item.has_text() {
        doc.append_blocks(&item.blocks)?;
    }
    doc.diff_since(&before)
}

fn unexpected(answer: Option<BackendEvent>) -> String {
    match answer {
        Some(BackendEvent::Error { message }) => message,
        Some(other) => format!("the backend answered {other:?}"),
        None => "the backend did not answer".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_crdt::Block;

    #[test]
    fn a_node_is_filled_in_one_update() {
        let mut created = NodeDoc::new();
        created.init(node_types::DOCUMENT, "Chapter", None, "2026-10-09T00:00:00Z").unwrap();
        let mut item = Imported::document("Chapter", vec![Block::plain("It was a dark night.")]);
        item.node_type = node_types::FOLDER;
        item.color = Some("#ff0080".into());
        item.background = Some("#b0d7ff".into());
        item.icon = Some("star".into());
        item.tags = vec!["Draft".into()];

        let delta = fill(&created.save(), &item).unwrap();
        created.apply_update(&delta).unwrap();

        let fields = created.fields().unwrap();
        assert_eq!(fields.node_type, node_types::FOLDER);
        assert_eq!(fields.tags, vec!["Draft".to_string()]);
        assert_eq!(fields.custom.get(custom_keys::COLOR), Some(&serde_json::json!("#ff0080")));
        assert_eq!(fields.custom.get(custom_keys::ICON), Some(&serde_json::json!("star")));
        assert_eq!(fields.custom.get(custom_keys::BACKGROUND), Some(&serde_json::json!("#b0d7ff")));
        assert_eq!(fields.custom.get(custom_keys::EXPLICIT_TITLE), Some(&serde_json::json!(true)));
        assert_eq!(created.text().trim(), "It was a dark night.");
    }
}
