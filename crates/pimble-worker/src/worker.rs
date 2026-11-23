use std::sync::Arc;

use pimble_client::message_port::{post_text, PortText};
use pimble_host::{Disk, MemoryKeyRest, OpfsDisk, SealedDisk};
use pimble_server::{service_extensions, InheritedDevice, PimbleServer, ServerConfig};
use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

use crate::device;

/// The running server, as the worker's script holds it.
#[wasm_bindgen]
pub struct WorkerServer {
    server: PimbleServer,
    remembered: bool,
}

/// Start the server with the worker's adapters (`ServerConfig::default()` on
/// wasm32: a memory disk, `LocalRuntime`, `fetch`, folders `/pimble` and
/// `/pimble-config`). The answer's `connect` serves a port.
#[wasm_bindgen(js_name = startServer)]
pub async fn start_server() -> Result<WorkerServer, JsValue> {
    start(ServerConfig::default()).await.map_err(|e| JsValue::from_str(&e.to_string()))
}

/// [`start_server`] with a configuration of the host's own (its disk, its
/// paths): what the browser's start and the tests call.
pub async fn start(config: ServerConfig) -> pimble_server::Result<WorkerServer> {
    console_error_panic_hook::set_once();
    let mut server = PimbleServer::with_config(config);
    server.start_in_process().await?;
    tracing::info!("Pimble server started in this worker");
    Ok(WorkerServer { server, remembered: false })
}

/// How the browser's server keeps what it holds (docs/ONE_CODEBASE_CONTRACT.md
/// section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keeping {
    /// Remembered when this browser already remembers itself, else memory:
    /// what a tab asks for before anyone has signed in.
    Auto,
    /// Remembered: a device key (made now if there is none), the disk key
    /// under it, OPFS sealed with the disk key, the keystore in IndexedDB.
    Remember,
    /// "Don't remember this browser": a memory disk and the keys in memory.
    Memory,
}

impl Keeping {
    pub fn parse(mode: &str) -> Self {
        match mode {
            "remember" => Keeping::Remember,
            "memory" => Keeping::Memory,
            _ => Keeping::Auto,
        }
    }
}

/// The browser's server (wave 4): kept as `mode` says, with the device id and
/// answered reports the page handed over from its `localStorage` (`inherited`,
/// JSON `{ "device": "...", "answered": { "<store id>": {...} } }`), and every
/// store of this browser's opened again. A remembered browser whose files do
/// not open (its device key gone or changed underneath) is forgotten and
/// starts on memory, said in the log.
#[wasm_bindgen(js_name = startBrowserServer)]
pub async fn start_browser_server(mode: String, inherited: Option<String>) -> Result<WorkerServer, JsValue> {
    start_browser(Keeping::parse(&mode), inherited.as_deref()).await.map_err(|e| JsValue::from_str(&e))
}

pub async fn start_browser(keeping: Keeping, inherited: Option<&str>) -> Result<WorkerServer, String> {
    let inherited = inherited.and_then(parse_inherited);
    let remember = match keeping {
        Keeping::Memory => false,
        Keeping::Remember => true,
        Keeping::Auto => device::is_remembered().await,
    };
    if remember {
        match remembered_config().await {
            Ok(config) => match start(ServerConfig { inherited_device: inherited.clone(), ..config }).await {
                Ok(mut server) => {
                    server.remembered = true;
                    server.open_stores().await;
                    return Ok(server);
                }
                Err(e) => tracing::error!("The server could not start on this browser's files ({e}); forgetting them and starting in memory"),
            },
            Err(e) => tracing::error!("This browser's files do not open ({e}); forgetting them and starting in memory"),
        }
        if let Err(e) = forget_browser().await {
            tracing::error!("Forgetting this browser failed: {e}");
        }
    }
    let config = ServerConfig { key_rest: Some(Arc::new(MemoryKeyRest::default())), inherited_device: inherited, ..ServerConfig::default() };
    start(config).await.map_err(|e| e.to_string())
}

async fn remembered_config() -> Result<ServerConfig, String> {
    if !OpfsDisk::available().await {
        return Err("this browser gives no origin private file system here".to_string());
    }
    let key = device::disk_key(true).await.map_err(|e| e.to_string())?.ok_or("no device key")?;
    let disk = Disk::new(SealedDisk::new(Arc::new(OpfsDisk), key));
    Ok(ServerConfig { disk, key_rest: Some(Arc::new(device::DeviceKeyRest)), ..ServerConfig::default() })
}

fn parse_inherited(json: &str) -> Option<InheritedDevice> {
    #[derive(serde::Deserialize)]
    struct Handed {
        device: String,
        #[serde(default)]
        answered: std::collections::HashMap<String, pimble_rpc::vault::Comparing>,
    }
    let handed: Handed = serde_json::from_str(json).map_err(|e| tracing::warn!("The device handed over from this page does not read ({e}); a new one is made")).ok()?;
    let answered = handed.answered.into_iter().filter_map(|(store, comparing)| Some((pimble_core::StoreId::parse(&store).ok()?, comparing))).collect();
    Some(InheritedDevice { device: handed.device, answered })
}

/// Delete the device key, then everything beside it, then every file in OPFS
/// (section 7: "Each deletes the device key first").
pub async fn forget_browser() -> Result<(), String> {
    let forgot = device::forget().await.map_err(|e| format!("the device key: {e}"));
    let cleared = if OpfsDisk::available().await { OpfsDisk::clear().await.map_err(|e| format!("its files: {e}")) } else { Ok(()) };
    forgot.and(cleared)
}

#[wasm_bindgen]
impl WorkerServer {
    /// Serve one connection on `port`, the worker's end of a
    /// `MessageChannel` whose other end is a page's `PimbleClient`: JSON-RPC
    /// text, one string per message, every call as the `Service` principal
    /// (the page is this browser's own app, as the desktop app is its
    /// embedded server's). Served until the port can no longer be written,
    /// or the other side says it has gone.
    pub fn connect(&self, port: MessagePort) -> Result<(), JsValue> {
        let incoming = PortText::listen(&port).into_stream();
        let send = move |text: String| post_text(&port, &text).is_ok();
        if self.server.serve_connection(service_extensions(), incoming, send) {
            Ok(())
        } else {
            Err(JsValue::from_str("the Pimble server has not started"))
        }
    }

    /// `"remembered"` or `"memory"`.
    pub fn mode(&self) -> String {
        if self.remembered { "remembered" } else { "memory" }.to_string()
    }

}

impl WorkerServer {
    /// Stop: every link stopped, every document written.
    pub async fn stop(&mut self) -> Result<(), String> {
        self.server.stop().await.map_err(|e| e.to_string())
    }

    /// Stop, then forget this browser: the device key, the database beside
    /// it, every file in OPFS. Answers whether OPFS is empty after.
    pub async fn forget(&mut self) -> Result<bool, String> {
        // The key first: whatever the stop writes after is already unreadable.
        let forgot = device::forget().await;
        let _ = self.server.stop().await;
        forget_browser().await?;
        forgot.map_err(|e| e.to_string())?;
        Ok(!OpfsDisk::available().await || OpfsDisk::root_names().await.map(|n| n.is_empty()).unwrap_or(false))
    }

    /// The server, for a Rust host (the tests) that wants more than ports.
    pub fn server(&self) -> &PimbleServer {
        &self.server
    }

    pub fn server_mut(&mut self) -> &mut PimbleServer {
        &mut self.server
    }

    /// Open every store this browser keeps (`/pimble/replicas/*.pimble`): a
    /// browser has no list of open stores but this one, since every store in
    /// it is the account's and kept here (section 5).
    async fn open_stores(&self) {
        let Some(client) = self.server.connect_in_process(service_extensions()) else { return };
        let disk = self.server.store_manager().read().await.disk().clone();
        let dir = pimble_host::Paths::new("/pimble", "/pimble-config").replicas();
        let Ok(entries) = disk.list(&dir).await else { return };
        let started = web_time::Instant::now();
        let mut opened = 0;
        for entry in entries {
            if !entry.is_dir || !entry.name().is_some_and(|n| n.ends_with(".pimble")) {
                continue;
            }
            let began = web_time::Instant::now();
            match client.open_store(&entry.path).await {
                Ok(store) => {
                    opened += 1;
                    tracing::info!("Opened {} ({}) from this browser's files in {} ms", store.name, store.id, began.elapsed().as_millis());
                }
                Err(e) => tracing::warn!("{} did not open: {e}", entry.path.display()),
            }
        }
        tracing::info!("{opened} stores opened from this browser's files in {} ms", started.elapsed().as_millis());
    }
}
