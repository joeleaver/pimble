use pimble_client::message_port::{post_text, PortText};
use pimble_server::{service_extensions, PimbleServer, ServerConfig};
use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

/// The running server, as the worker's script holds it.
#[wasm_bindgen]
pub struct WorkerServer {
    server: PimbleServer,
}

/// Start the server with the worker's adapters (`ServerConfig::default()` on
/// wasm32: a memory disk, `LocalRuntime`, `fetch`, folders `/pimble` and
/// `/pimble-config`). The answer's `connect` serves a port.
#[wasm_bindgen(js_name = startServer)]
pub async fn start_server() -> Result<WorkerServer, JsValue> {
    start(ServerConfig::default()).await.map_err(|e| JsValue::from_str(&e.to_string()))
}

/// [`start_server`] with a configuration of the host's own (its disk, its
/// paths): what wave 4's OPFS start and the tests call.
pub async fn start(config: ServerConfig) -> pimble_server::Result<WorkerServer> {
    console_error_panic_hook::set_once();
    let mut server = PimbleServer::with_config(config);
    server.start_in_process().await?;
    tracing::info!("Pimble server started in this worker");
    Ok(WorkerServer { server })
}

#[wasm_bindgen]
impl WorkerServer {
    /// Serve one connection on `port`, the worker's end of a
    /// `MessageChannel` whose other end is a page's `PimbleClient`: JSON-RPC
    /// text, one string per message, every call as the `Service` principal
    /// (the page is this browser's own app, as the desktop app is its
    /// embedded server's). Served until the port can no longer be written.
    pub fn connect(&self, port: MessagePort) -> Result<(), JsValue> {
        let incoming = PortText::listen(&port).into_stream();
        let send = move |text: String| post_text(&port, &text).is_ok();
        if self.server.serve_connection(service_extensions(), incoming, send) {
            Ok(())
        } else {
            Err(JsValue::from_str("the Pimble server has not started"))
        }
    }
}

impl WorkerServer {
    /// The server, for a Rust host (the tests) that wants more than ports.
    pub fn server(&self) -> &PimbleServer {
        &self.server
    }

    pub fn server_mut(&mut self) -> &mut PimbleServer {
        &mut self.server
    }
}
