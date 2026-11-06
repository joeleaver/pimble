//! A plugin's tools, run for an agent with no window (docs/AGENT_GUIDES_CONTRACT.md
//! section 8): the module is instantiated with no document, the tool started
//! through `pimble_tool`, and its task driven through the same exports a
//! mounted view runs on (`poll_task`, `fire_timeout`, `drain_polls`, `resolve`)
//! until `pimble_tool_result` answers.
//!
//! Its API calls are made through [`crate::bridge::execute`] with the approval
//! the plugin's own manifest asks for (`agent_approval`), over the agent's
//! backend, so the server judges each one by the agent's grant: the tool can
//! do no more than both allow. A tool that draws is stopped.
//!
//! Everything here runs on one thread, inside a `tokio::task::LocalSet`.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pimble_api::Backend;
use pimble_plugin_abi::api::{decode_json, encode_json, ApiCall, Resolution};
use pimble_plugin_abi::guide::{ToolAnswer, ToolRequest};
use pimble_plugin_abi::{decode, encode, Answer, Manifest, Op, StoreAsk};

use crate::bridge::{execute, Approval, Approved};
use crate::instance::{forget_host, register_host, Answered, Call, Failure, HostCalls};
use crate::wasmtime_engine::{Plugin, DEFAULT_BUDGET};
use crate::Refusal;

/// How long one tool run may take, API calls and all.
pub const TOOL_DEADLINE: Duration = Duration::from_secs(30);

/// What a plugin's tool may do for an agent: what its manifest asks for, every
/// store it asks for ("chosen" is the agent's to choose, which its grant
/// already says), and writes only when it asks to write. The server still
/// judges every call by the agent's grant.
pub fn agent_approval(manifest: &Manifest) -> Approval {
    let stores = match manifest.asks.stores {
        StoreAsk::None => Approved::Nothing,
        StoreAsk::All | StoreAsk::Chosen => Approved::All,
    };
    Approval { stores, write: manifest.asks.write }
}

/// The imports of a module running a tool.
struct Headless {
    plugin_id: String,
    name: String,
    approval: Approval,
    backend: Arc<dyn Backend>,
    tasks: RefCell<VecDeque<u64>>,
    timers: RefCell<Vec<(Instant, u64)>>,
    polls: Cell<bool>,
    resolved: Rc<RefCell<VecDeque<Vec<u8>>>>,
    wake: Rc<tokio::sync::Notify>,
    last_error: RefCell<Option<String>>,
}

impl HostCalls for Headless {
    fn flush(&self, ops: &[u8]) -> Result<(), Refusal> {
        match decode::<Vec<Op>>(ops) {
            Ok(ops) if ops.is_empty() => Ok(()),
            _ => Err(Refusal("a tool draws nothing".into())),
        }
    }

    fn query(&self, _query: &[u8]) -> Vec<u8> {
        encode(&Answer::Nothing)
    }

    fn set_timeout(&self, id: u64, delay_ms: u32) {
        self.timers.borrow_mut().push((Instant::now() + Duration::from_millis(delay_ms.into()), id));
        self.wake.notify_one();
    }

    fn schedule_task(&self, id: u64) {
        self.tasks.borrow_mut().push_back(id);
        self.wake.notify_one();
    }

    fn wake_polls(&self) {
        self.polls.set(true);
        self.wake.notify_one();
    }

    fn log(&self, level: u32, line: &str) {
        if level == 0 {
            *self.last_error.borrow_mut() = Some(line.to_string());
            tracing::warn!("plugin {} (tool): {line}", self.plugin_id);
        } else {
            tracing::debug!("plugin {} (tool): {line}", self.plugin_id);
        }
    }

    fn api_call(&self, call: u64, bytes: &[u8]) {
        let (backend, plugin_id, name, approval) = (self.backend.clone(), self.plugin_id.clone(), self.name.clone(), self.approval.clone());
        let (resolved, wake) = (self.resolved.clone(), self.wake.clone());
        let request = decode_json::<ApiCall>(bytes);
        tokio::task::spawn_local(async move {
            let reply = match request {
                Ok(request) => execute(&*backend, &plugin_id, &name, &approval, request).await,
                Err(e) => pimble_plugin_abi::api::ApiReply::error(pimble_api::ApiError::Refused { sentence: format!("a call the host could not read: {e}") }),
            };
            resolved.borrow_mut().push_back(encode_json(&Resolution { call, reply }));
            wake.notify_one();
        });
    }

    fn api_cancel(&self, _call: u64) {}

    fn host_request(&self, _bytes: &[u8]) {}
}

/// Run `request` in `plugin` for an agent, through `backend`. The answer is
/// the tool's own; an error is a sentence saying why the plugin could not
/// answer (it has no tools, it stopped, it ran past [`TOOL_DEADLINE`]).
/// Call inside a `LocalSet`.
pub async fn run(plugin: &Plugin, backend: Arc<dyn Backend>, request: &ToolRequest) -> Result<ToolAnswer, String> {
    let name = plugin.manifest.name.clone();
    let host = Rc::new(Headless {
        plugin_id: plugin.manifest.id.clone(),
        name: name.clone(),
        approval: agent_approval(&plugin.manifest),
        backend,
        tasks: RefCell::new(VecDeque::new()),
        timers: RefCell::new(Vec::new()),
        polls: Cell::new(false),
        resolved: Rc::new(RefCell::new(VecDeque::new())),
        wake: Rc::new(tokio::sync::Notify::new()),
        last_error: RefCell::new(None),
    });
    let calls: Rc<dyn HostCalls> = host.clone();
    let key = register_host(&calls);
    let answered = drive(plugin, key, &host, request).await;
    forget_host(key);
    answered.map_err(|failure| match failure {
        Failure::Trapped(why) => {
            let why = host.last_error.borrow().clone().unwrap_or(why);
            Failure::Trapped(why).sentence(&name)
        }
        other => other.sentence(&name),
    })
}

async fn drive(plugin: &Plugin, key: u64, host: &Rc<Headless>, request: &ToolRequest) -> Result<ToolAnswer, Failure> {
    let mut engine = plugin.instantiate(key, DEFAULT_BUDGET)?;
    let bytes = serde_json::to_vec(request).map_err(|e| Failure::Refused(e.to_string()))?;
    let id = match engine.call(Call::Tool(&bytes))? {
        Answered::Number(id) => id as u64,
        other => return Err(Failure::Refused(format!("it answered a tool's start with {other:?}"))),
    };
    let deadline = Instant::now() + TOOL_DEADLINE;
    loop {
        if let Answered::Bytes(bytes) = engine.call(Call::ToolResult(id))? {
            return serde_json::from_slice(&bytes).map_err(|e| Failure::Refused(format!("its answer does not read: {e}")));
        }
        let mut did = false;
        loop {
            let next = host.tasks.borrow_mut().pop_front();
            let Some(task) = next else { break };
            engine.call(Call::PollTask(task))?;
            did = true;
        }
        loop {
            let next = host.resolved.borrow_mut().pop_front();
            let Some(resolution) = next else { break };
            engine.call(Call::Resolve(&resolution))?;
            did = true;
        }
        let now = Instant::now();
        let due: Vec<u64> = {
            let mut timers = host.timers.borrow_mut();
            let (due, later): (Vec<_>, Vec<_>) = timers.drain(..).partition(|(at, _)| *at <= now);
            *timers = later;
            due.into_iter().map(|(_, id)| id).collect()
        };
        for id in due {
            engine.call(Call::FireTimeout(id))?;
            did = true;
        }
        if host.polls.replace(false) {
            engine.call(Call::DrainPolls)?;
            did = true;
        }
        if did {
            continue;
        }
        if now >= deadline {
            return Err(Failure::TooLong);
        }
        let next_timer = host.timers.borrow().iter().map(|(at, _)| *at).min();
        let wait = next_timer.unwrap_or(deadline).min(deadline).saturating_duration_since(now);
        tokio::select! {
            _ = host.wake.notified() => {}
            _ = tokio::time::sleep(wait) => {}
        }
    }
}
