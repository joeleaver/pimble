use super::library_client::{forget_library, install_failed, installed_from_library};
use super::window::say;
use super::*;
use pimble_plugin_host::js_engine::{load_bytes, DEFAULT_BUDGET};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

pub const CAN_ADD: bool = true;
pub const REMOVABLE: bool = true;
pub const WHERE: &str = "Plugins added here are kept in this browser only, never uploaded. Each asks once in this browser before it reads or changes anything.";

const APPROVALS_KEY: &str = "pimble.plugins.approvals";

// The page's own store of plugins: one IndexedDB object store, a
// plugin's module bytes under its id.
#[wasm_bindgen(inline_js = r#"
const DB = 'pimble-plugins', STORE = 'plugins';
function open() {
  return new Promise((ok, fail) => {
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => r.result.createObjectStore(STORE);
    r.onsuccess = () => ok(r.result);
    r.onerror = () => fail(r.error);
  });
}
function done(tx) {
  return new Promise((ok, fail) => {
    tx.oncomplete = () => ok();
    tx.onerror = () => fail(tx.error);
    tx.onabort = () => fail(tx.error);
  });
}
export async function pluginsList() {
  const db = await open();
  const tx = db.transaction(STORE, 'readonly');
  const out = [];
  await new Promise((ok, fail) => {
    const c = tx.objectStore(STORE).openCursor();
    c.onsuccess = () => {
      const cur = c.result;
      if (cur) { out.push([String(cur.key), new Uint8Array(cur.value)]); cur.continue(); } else { ok(); }
    };
    c.onerror = () => fail(c.error);
  });
  db.close();
  return out;
}
export async function pluginsPut(id, bytes) {
  const db = await open();
  const tx = db.transaction(STORE, 'readwrite');
  tx.objectStore(STORE).put(bytes.slice().buffer, id);
  await done(tx);
  db.close();
}
export async function libraryFetch(url) {
  let r;
  try { r = await fetch(url, { credentials: 'omit' }); } catch (e) { throw String(e); }
  if (!r.ok) throw 'HTTP ' + r.status;
  return new Uint8Array(await r.arrayBuffer());
}
export async function pluginsDelete(id) {
  const db = await open();
  const tx = db.transaction(STORE, 'readwrite');
  tx.objectStore(STORE).delete(id);
  await done(tx);
  db.close();
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = pluginsList, catch)]
    async fn plugins_list() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = pluginsPut, catch)]
    async fn plugins_put(id: &str, bytes: &[u8]) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = pluginsDelete, catch)]
    async fn plugins_delete(id: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = libraryFetch, catch)]
    async fn library_fetch(url: &str) -> Result<JsValue, JsValue>;
}

const PROVENANCE_KEY: &str = "pimble.plugins.library";

pub fn read_provenance() -> Provenance {
    storage().and_then(|s| s.get_item(PROVENANCE_KEY).ok().flatten()).map(|json| Provenance::parse(json.as_bytes())).unwrap_or_default()
}

pub fn write_provenance(json: &[u8]) {
    let saved = storage().map(|s| s.set_item(PROVENANCE_KEY, &String::from_utf8_lossy(json)));
    if !matches!(saved, Some(Ok(()))) {
        tracing::warn!("plugins: where the plugins came from could not be saved in this browser");
    }
}

/// The library of the service this page came from.
pub fn library_base() -> String {
    "/api/v1/plugins".to_string()
}

pub fn fetch(url: String, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
    wasm_bindgen_futures::spawn_local(async move {
        let fetched = match library_fetch(&url).await {
            Ok(bytes) => Ok(js_sys::Uint8Array::new(&bytes).to_vec()),
            Err(e) => Err(e.as_string().unwrap_or_else(|| format!("{e:?}"))),
        };
        done(fetched);
    });
}

/// A verified download from the library, kept as "Add from a file..."
/// keeps one, once it loads as the plugin the index listed.
pub fn install(entry: Entry, bytes: Vec<u8>) {
    wasm_bindgen_futures::spawn_local(async move {
        match put(&entry.wasm, &bytes, Some(&entry)).await {
            Ok(_) => installed_from_library(&entry),
            Err(e) => install_failed(&entry, e),
        }
    });
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

pub fn read_approvals() -> HashMap<String, Approval> {
    storage().and_then(|s| s.get_item(APPROVALS_KEY).ok().flatten()).map(|json| parse_approvals(json.as_bytes())).unwrap_or_default()
}

pub fn write_approvals(json: &[u8]) {
    let saved = storage().map(|s| s.set_item(APPROVALS_KEY, &String::from_utf8_lossy(json)));
    if !matches!(saved, Some(Ok(()))) {
        tracing::warn!("plugins: the approvals could not be saved in this browser");
    }
}

/// Every plugin kept in this browser, compiled and described.
pub fn load() {
    wasm_bindgen_futures::spawn_local(async {
        let mut registry = Registry::default();
        match plugins_list().await {
            Ok(list) => {
                for entry in js_sys::Array::from(&list).iter() {
                    let entry = js_sys::Array::from(&entry);
                    let id = entry.get(0).as_string().unwrap_or_default();
                    let bytes = js_sys::Uint8Array::new(&entry.get(1)).to_vec();
                    let loaded = load_bytes(&format!("{id}.wasm"), &bytes, DEFAULT_BUDGET).await;
                    registry.take(format!("{id}.wasm").into(), loaded);
                }
            }
            Err(e) => tracing::warn!("plugins: this browser's plugins could not be read: {e:?}"),
        }
        tracing::info!("plugins: {} loaded and {} refused in this browser", registry.plugins.len(), registry.refused.len());
        loaded(registry);
    });
}

/// Whether an instance is ready to mount now; if not, one is made and every
/// pane showing a metanode looks again once it is.
pub fn ready(plugin: &Plugin, store: AppStore) -> bool {
    // One that could not be made is taken anyway, so the pane says why.
    if plugin.is_ready() || plugin.is_broken() {
        return true;
    }
    plugin.warm(move || crate::metanodes::plugins_ready(store));
    false
}

pub fn instantiate(plugin: &Plugin, key: HostKey) -> Result<Box<dyn GuestEngine>, Failure> {
    plugin.instantiate(key)
}

/// "Add plugin...": a `.wasm` file the person picks, compiled and described
/// here; one this app runs is kept in this browser, replacing an earlier
/// version of the same plugin, and one it does not is refused with its
/// sentence and not kept.
pub fn pick() {
    const INPUT_ID: &str = "pimble-plugin-input";
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    if let Some(old) = document.get_element_by_id(INPUT_ID) {
        old.remove();
    }
    let Some(input) = document.create_element("input").ok().and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok()) else {
        return;
    };
    input.set_id(INPUT_ID);
    input.set_type("file");
    input.set_accept(".wasm,application/wasm");
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
            say(format!("Adding {name}..."));
            let bytes = match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => js_sys::Uint8Array::new(&buffer).to_vec(),
                Err(_) => return say(format!("{name} could not be read.")),
            };
            add(&name, &bytes).await;
        });
    });
    if input.add_event_listener_with_callback("change", on_change.unchecked_ref()).is_err() {
        return;
    }
    input.click();
}

async fn add(name: &str, bytes: &[u8]) {
    match put(name, bytes, None).await {
        Ok((id, line)) => {
            forget_library(&id);
            say(line);
        }
        Err(e) => say(e),
    }
}

/// Compile and describe `bytes`; one this app runs (and, from the
/// library, the one `entry` lists) is kept in this browser, replacing an
/// earlier version of the same plugin, and swapped in.
async fn put(name: &str, bytes: &[u8], entry: Option<&Entry>) -> Result<(String, String), String> {
    let plugin = load_bytes(name, bytes, DEFAULT_BUDGET).await.map_err(|(_, sentence)| sentence)?;
    if let Some(entry) = entry {
        library::matches(entry, &plugin.manifest)?;
    }
    let id = plugin.manifest.id.clone();
    if let Err(e) = plugins_put(&id, bytes).await {
        tracing::warn!("plugins: {id} could not be kept: {e:?}");
        return Err(format!("The {} plugin could not be kept in this browser.", plugin.manifest.name));
    }
    // An earlier version of the same plugin goes, with any pane showing it.
    let mut registry = REGISTRY.with(|r| r.borrow().clone()).unwrap_or_default();
    registry.plugins.retain(|p| p.manifest.id != id);
    registry.refused.retain(|r| r.file() != format!("{id}.wasm"));
    let line = format!("Added the {} plugin {}.", plugin.manifest.name, plugin.manifest.version);
    registry.take(format!("{id}.wasm").into(), Ok(plugin));
    swapped(registry, std::slice::from_ref(&id));
    Ok((id, line))
}

/// "Remove": the plugin leaves this browser, with what it was allowed.
pub fn remove(id: &str) {
    let id = id.to_string();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = plugins_delete(&id).await {
            tracing::warn!("plugins: {id} could not be removed: {e:?}");
            return say("The plugin could not be removed from this browser.".to_string());
        }
        let name = manifest_of(&id).map_or(id.clone(), |m| m.name);
        APPROVALS.with(|a| a.borrow_mut().remove(&id));
        write_approvals_now();
        forget_library(&id);
        let mut registry = REGISTRY.with(|r| r.borrow().clone()).unwrap_or_default();
        registry.plugins.retain(|p| p.manifest.id != id);
        swapped(registry, std::slice::from_ref(&id));
        say(format!("Removed the {name} plugin from this browser."));
    });
}

fn write_approvals_now() {
    super::write_approvals();
}
