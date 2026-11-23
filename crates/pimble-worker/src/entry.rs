//! The worker's own script (feature `entry`, what the page's build turns on):
//! the dedicated worker the leader tab starts runs this on load and answers
//! the page's messages (docs/ONE_CODEBASE_CONTRACT.md section 4).
//!
//! The page's side is `web/hub.js`. Its messages, each an object with `t`:
//!
//! - `start { mode, inherited }`: start the server ([`crate::start_browser`]);
//!   answered `started { mode }` (`remembered` or `memory`) or
//!   `failed { message }`.
//! - `connect`, with one `MessagePort` transferred: serve a connection on it
//!   (held until the server has started).
//! - `stop`: write everything and stop; answered `stopped`.
//! - `forget`: forget this browser; answered `forgotten { empty }` (whether
//!   OPFS is empty after) or `failed { message }`.

use std::cell::RefCell;

use js_sys::Reflect;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent, MessagePort};

use crate::worker::{start_browser, Keeping, WorkerServer};

#[derive(Default)]
struct State {
    server: Option<WorkerServer>,
    /// Ports that arrived before the server had started.
    waiting: Vec<MessagePort>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn scope() -> DedicatedWorkerGlobalScope {
    js_sys::global().unchecked_into()
}

fn answer(fields: &[(&str, JsValue)]) {
    let message = js_sys::Object::new();
    for (key, value) in fields {
        let _ = Reflect::set(&message, &JsValue::from_str(key), value);
    }
    let _ = scope().post_message(&message);
}

fn field(data: &JsValue, name: &str) -> Option<String> {
    Reflect::get(data, &JsValue::from_str(name)).ok().and_then(|v| v.as_string())
}

#[wasm_bindgen(start)]
pub fn entry() {
    console_error_panic_hook::set_once();
    install_tracing();
    let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| on_message(event));
    scope().set_onmessage(Some(listener.as_ref().unchecked_ref()));
    listener.forget();
    answer(&[("t", JsValue::from_str("loaded"))]);
}

/// The server's log in the worker's console, at INFO (the page's level).
fn install_tracing() {
    use tracing_subscriber::prelude::*;
    let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Console));
}

/// Each event at INFO or above as one console line: level, where, message
/// and fields.
struct Console;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Console {
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _: tracing_subscriber::layer::Context<'_, S>) -> bool {
        *metadata.level() <= tracing::Level::INFO
    }

    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Line(String);
        impl tracing::field::Visit for Line {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0.push_str(&format!(" {value:?}"));
                } else {
                    self.0.push_str(&format!(" {}={value:?}", field.name()));
                }
            }
        }
        let meta = event.metadata();
        let mut line = Line(format!("{} {}:", meta.level(), meta.module_path().unwrap_or(meta.target())));
        event.record(&mut line);
        let text = JsValue::from_str(&line.0);
        match *meta.level() {
            tracing::Level::ERROR => web_sys::console::error_1(&text),
            tracing::Level::WARN => web_sys::console::warn_1(&text),
            _ => web_sys::console::log_1(&text),
        }
    }
}

fn on_message(event: MessageEvent) {
    let data = event.data();
    match field(&data, "t").as_deref() {
        Some("start") => {
            let keeping = Keeping::parse(&field(&data, "mode").unwrap_or_default());
            let inherited = field(&data, "inherited");
            spawn_local(async move {
                if let Some(mode) = STATE.with(|s| s.borrow().server.as_ref().map(|s| s.mode())) {
                    answer(&[("t", "started".into()), ("mode", mode.into())]);
                    return;
                }
                let started = web_time::Instant::now();
                match start_browser(keeping, inherited.as_deref()).await {
                    Ok(server) => {
                        let mode = server.mode();
                        tracing::info!("The browser's server is up ({mode}) in {} ms", started.elapsed().as_millis());
                        for port in STATE.with(|s| std::mem::take(&mut s.borrow_mut().waiting)) {
                            let _ = server.connect(port);
                        }
                        STATE.with(|s| s.borrow_mut().server = Some(server));
                        answer(&[("t", "started".into()), ("mode", mode.into()), ("ms", (started.elapsed().as_millis() as f64).into())]);
                    }
                    Err(message) => answer(&[("t", "failed".into()), ("message", message.into())]),
                }
            });
        }
        Some("connect") => {
            let Some(port) = event.ports().get(0).dyn_into::<MessagePort>().ok() else { return };
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                match &s.server {
                    Some(server) => {
                        let _ = server.connect(port);
                    }
                    None => s.waiting.push(port),
                }
            });
        }
        Some("stop") => {
            // Out of the state first: nothing else reaches it while it stops.
            let server = STATE.with(|s| s.borrow_mut().server.take());
            spawn_local(async move {
                if let Some(mut server) = server {
                    let _ = server.stop().await;
                }
                answer(&[("t", "stopped".into())]);
            });
        }
        Some("forget") => {
            let server = STATE.with(|s| s.borrow_mut().server.take());
            spawn_local(async move {
                let forgotten = match server {
                    Some(mut server) => server.forget().await,
                    None => crate::worker::forget_browser().await.map(|_| true),
                };
                match forgotten {
                    Ok(empty) => answer(&[("t", "forgotten".into()), ("empty", empty.into())]),
                    Err(message) => answer(&[("t", "failed".into()), ("message", message.into())]),
                }
            });
        }
        other => tracing::warn!("The worker was sent a message it does not know: {other:?}"),
    }
}
