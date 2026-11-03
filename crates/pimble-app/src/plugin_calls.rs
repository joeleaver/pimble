//! Plugins' calls through the Pimble API, on whichever backend this build has
//! (docs/PLUGINS_CONTRACT.md waves 2 and 3): the desktop's backend thread runs
//! them over the window's own `Pimble`, the browser's backend loop over a
//! router of its own whose encrypted stores are the page's vault.
//!
//! Each call is a task of its own beside the command loop, so a slow one (or a
//! subscription, which lasts) holds none of the window's commands up; what a
//! cancelled call or a stopped plugin leaves in flight is aborted.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;
use futures::future::AbortHandle;
use pimble_api::{Backend, MaybeSend, MaybeSync, Pimble, Router};
use pimble_plugin_host::abi::api::{ApiCall, ApiReply};
use pimble_plugin_host::bridge;

use crate::protocol::{BackendCommand, BackendEvent};

/// What a plugin's calls are made through: the window's own API, or a router.
pub trait CallsThrough: MaybeSend + MaybeSync + 'static {
    fn backend(&self) -> &dyn Backend;
}

impl CallsThrough for Pimble {
    fn backend(&self) -> &dyn Backend {
        Pimble::backend(self)
    }
}

impl CallsThrough for Router {
    fn backend(&self) -> &dyn Backend {
        self
    }
}

/// The plugin calls in flight, by instance and call.
#[derive(Default, Clone)]
pub struct PluginCalls(Arc<Mutex<HashMap<u64, HashMap<u64, AbortHandle>>>>);

impl PluginCalls {
    /// Run `cmd` when it is a plugin's, through `through`, answering `None`;
    /// any other command comes back for the loop.
    pub fn take<T: CallsThrough>(
        &self,
        cmd: BackendCommand,
        through: &Arc<T>,
        event_tx: &Sender<BackendEvent>,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Option<BackendCommand> {
        match cmd {
            BackendCommand::PluginCall { instance, plugin_id, plugin_name, approval, call_id, call } => {
                let (through, event_tx, signal_ui, calls) = (through.clone(), event_tx.clone(), signal_ui.clone(), self.clone());
                let reply = move |reply: ApiReply| {
                    let _ = event_tx.try_send(BackendEvent::PluginReply { instance, call_id, reply });
                    signal_ui();
                };
                let work = async move {
                    let backend = through.backend();
                    match call {
                        ApiCall::Subscribe { store } => match bridge::subscribe(backend, &plugin_name, &approval, store).await {
                            Ok(mut changes) => {
                                use futures::StreamExt;
                                reply(ApiReply::Subscribed);
                                while let Some(change) = changes.next().await {
                                    reply(ApiReply::Change { change: change.into() });
                                }
                                reply(ApiReply::Ended);
                            }
                            Err(e) => reply(ApiReply::error(e)),
                        },
                        call => reply(bridge::execute(backend, &plugin_id, &plugin_name, &approval, call).await),
                    }
                    calls.done(instance, call_id);
                };
                let (work, handle) = futures::future::abortable(work);
                self.0.lock().unwrap().entry(instance).or_default().insert(call_id, handle);
                spawn(async move {
                    let _ = work.await;
                });
                None
            }
            BackendCommand::PluginCancel { instance, call_id } => {
                if let Some(handle) = self.0.lock().unwrap().get_mut(&instance).and_then(|calls| calls.remove(&call_id)) {
                    handle.abort();
                }
                None
            }
            BackendCommand::PluginForget { instance } => {
                for (_, handle) in self.0.lock().unwrap().remove(&instance).unwrap_or_default() {
                    handle.abort();
                }
                None
            }
            other => Some(other),
        }
    }

    fn done(&self, instance: u64, call_id: u64) {
        let mut all = self.0.lock().unwrap();
        if let Some(calls) = all.get_mut(&instance) {
            calls.remove(&call_id);
            if calls.is_empty() {
                all.remove(&instance);
            }
        }
    }
}

/// Native has a tokio runtime to put a call on; the browser has the page's own
/// task queue, where nothing is `Send` and nothing needs to be.
#[cfg(feature = "native")]
fn spawn(work: impl std::future::Future<Output = ()> + Send + 'static) {
    tokio::spawn(work);
}

#[cfg(not(feature = "native"))]
fn spawn(work: impl std::future::Future<Output = ()> + 'static) {
    wasm_bindgen_futures::spawn_local(work);
}
