//! One running plugin: its engine, its applier, its timers, and how it stops
//! (docs/PLUGINS_CONTRACT.md section 3.2).
//!
//! Engine-agnostic. A [`GuestEngine`] makes the export calls (wasmtime on the
//! desktop; the browser's own WebAssembly in wave 3) and services the guest's
//! imports through the [`HostCalls`] it finds under the instance's [`HostKey`]. An
//! engine cannot hold the instance itself (wasmtime's store data must be `Send`,
//! and the instance holds the app's document), so it holds the key and looks the
//! instance up on the UI thread, where every call happens.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};

use pimble_plugin_abi::{decode, encode, Event, EventData, MountArgs, Op, Props, Query};
use rinch_core::dom::{DomDocument, NodeId};

use crate::applier::{Applier, ApplierOptions, Dispatch, Refusal};

/// An export call.
#[derive(Debug, Clone, Copy)]
pub enum Call<'a> {
    Describe,
    Mount(&'a [u8]),
    Dispatch(&'a [u8]),
    PropsChanged(&'a [u8]),
    Unmount,
    FireTimeout(u64),
    PollTask(u64),
    DrainPolls,
}

/// What an export call answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Answered {
    Done,
    Bytes(Vec<u8>),
    /// `dispatch`'s "consumed".
    Flag(bool),
    /// `drain_polls`'s milliseconds to the next poll.
    Number(i64),
}

/// Why an export call failed. Any failure stops the instance.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// It ran past its budget and was interrupted.
    TooLong,
    /// The host refused something it sent (a node not its own, bytes that are not
    /// a message).
    Refused(String),
    /// It trapped: a panic, an access out of bounds, a missing export.
    Trapped(String),
}

impl Failure {
    /// The sentence a person reads in place of the plugin.
    pub fn sentence(&self, plugin: &str) -> String {
        match self {
            Failure::TooLong => format!("The {plugin} plugin was stopped: it ran too long without answering."),
            Failure::Refused(why) => format!("The {plugin} plugin was stopped: {why}."),
            Failure::Trapped(why) => format!("The {plugin} plugin stopped working: {why}."),
        }
    }
}

/// Makes a plugin's export calls.
pub trait GuestEngine {
    fn call(&mut self, call: Call<'_>) -> Result<Answered, Failure>;
}

/// What a guest's imports reach.
pub trait HostCalls {
    /// Apply a list of ops (postcard bytes). A refusal traps the guest.
    fn flush(&self, ops: &[u8]) -> Result<(), Refusal>;
    /// Answer a query (postcard bytes in and out).
    fn query(&self, query: &[u8]) -> Vec<u8>;
    fn set_timeout(&self, id: u64, delay_ms: u32);
    fn schedule_task(&self, id: u64);
    fn wake_polls(&self);
    fn log(&self, level: u32, line: &str);
}

/// Names an instance's [`HostCalls`] to its engine.
pub type HostKey = u64;

thread_local! {
    static HOSTS: RefCell<HashMap<HostKey, Weak<dyn HostCalls>>> = RefCell::new(HashMap::new());
    static NEXT_KEY: Cell<HostKey> = const { Cell::new(1) };
}

/// The instance an engine's imports reach, if it is still alive. Key 0 is no
/// instance: a module being asked to describe itself.
pub fn host_calls(key: HostKey) -> Option<Rc<dyn HostCalls>> {
    HOSTS.with(|h| h.borrow().get(&key).and_then(Weak::upgrade))
}

/// What a plugin instance needs to run.
#[derive(Clone, Copy, Debug)]
pub struct InstanceOptions {
    pub applier: ApplierOptions,
}

struct Inner {
    /// The plugin's name, for sentences and the log.
    name: String,
    key: HostKey,
    engine: RefCell<Option<Box<dyn GuestEngine>>>,
    applier: RefCell<Applier>,
    /// Events that arrived while the guest was busy (a write it made set off a
    /// host handler of its own): handed over once the call returns.
    queued: RefCell<VecDeque<Event>>,
    stopped: Cell<bool>,
    on_stop: RefCell<Option<Box<dyn Fn(&str)>>>,
    /// The last error the plugin logged (its panic message), for the sentence when
    /// it traps.
    last_error: RefCell<Option<String>>,
    /// A poll drain is armed.
    polls_armed: Cell<bool>,
    /// This instance, for the timers it arms.
    me: Weak<Inner>,
}

/// A running plugin, writing under one container of the app's document.
#[derive(Clone)]
pub struct Instance(Rc<Inner>);

impl Instance {
    /// Start plugin `name` under `container`, with an engine `make` builds for the
    /// instance's key. The engine is made after the instance exists, so imports made
    /// while it instantiates already reach it.
    pub fn start(
        name: &str,
        doc: Rc<RefCell<dyn DomDocument>>,
        container: NodeId,
        options: InstanceOptions,
        make: impl FnOnce(HostKey) -> Result<Box<dyn GuestEngine>, Failure>,
    ) -> Result<Instance, String> {
        let key = NEXT_KEY.with(|k| {
            let key = k.get();
            k.set(key + 1);
            key
        });
        let inner = Rc::new_cyclic(|me: &Weak<Inner>| {
            let weak = me.clone();
            let dispatch: Dispatch = Rc::new(move |event: Event| weak.upgrade().is_some_and(|inner| inner.dispatch(event)));
            Inner {
                name: name.to_string(),
                key,
                engine: RefCell::new(None),
                applier: RefCell::new(Applier::new(doc, container, dispatch, options.applier)),
                queued: RefCell::new(VecDeque::new()),
                stopped: Cell::new(false),
                on_stop: RefCell::new(None),
                last_error: RefCell::new(None),
                polls_armed: Cell::new(false),
                me: me.clone(),
            }
        });
        let calls: Rc<dyn HostCalls> = inner.clone();
        HOSTS.with(|h| h.borrow_mut().insert(key, Rc::downgrade(&calls)));
        match make(key) {
            Ok(engine) => {
                *inner.engine.borrow_mut() = Some(engine);
                Ok(Instance(inner))
            }
            Err(failure) => Err(failure.sentence(name)),
        }
    }

    /// Call `f` with the sentence when the instance stops by itself (it ran too
    /// long, was refused, trapped). Not called for [`Instance::unmount`].
    pub fn on_stop(&self, f: impl Fn(&str) + 'static) {
        *self.0.on_stop.borrow_mut() = Some(Box::new(f));
    }

    pub fn is_stopped(&self) -> bool {
        self.0.stopped.get()
    }

    /// The plugin's name.
    pub fn name(&self) -> &str {
        &self.0.name
    }

    /// Mount viewer `viewer` showing `props`. False when the plugin stopped.
    pub fn mount(&self, viewer: &str, props: &Props) -> bool {
        let args = encode(&MountArgs { viewer: viewer.to_string(), props: props.clone() });
        self.0.call(Call::Mount(&args)).is_some()
    }

    /// Hand the mounted view new props.
    pub fn props_changed(&self, props: &Props) -> bool {
        let bytes = encode(props);
        self.0.call(Call::PropsChanged(&bytes)).is_some()
    }

    /// Hand an event to the guest, as a proxy handler does; answers "consumed".
    pub fn dispatch(&self, event: Event) -> bool {
        self.0.dispatch(event)
    }

    /// Take the view down and stop the instance: the guest unmounts, then whatever
    /// it left in the container is discarded and its handlers unregistered.
    pub fn unmount(&self) {
        if !self.0.stopped.get() {
            let _ = self.0.call(Call::Unmount);
        }
        self.0.halt();
    }

    /// The host node a guest node stands for (tests).
    pub fn host_node(&self, guest: u64) -> Option<NodeId> {
        self.0.applier.borrow().host_node(guest)
    }

    /// The guest node a host node stands for (tests).
    pub fn guest_node(&self, host: NodeId) -> Option<u64> {
        self.0.applier.borrow().guest_node(host)
    }

    /// How many host handlers stand for the guest's (tests).
    pub fn proxy_count(&self) -> usize {
        self.0.applier.borrow().proxy_count()
    }

    /// Apply ops as if the guest had flushed them (tests of the sandbox).
    pub fn apply_for_test(&self, ops: Vec<Op>) -> Result<(), Refusal> {
        self.0.flush(&encode(&ops))
    }
}

impl Inner {
    /// Make an export call. `None` when the instance has stopped, the call failed
    /// (which stops it), or the guest is in a call already.
    fn call(&self, call: Call<'_>) -> Option<Answered> {
        if self.stopped.get() {
            return None;
        }
        let result = {
            let Ok(mut engine) = self.engine.try_borrow_mut() else {
                tracing::debug!("plugin {}: a call arrived while it was busy", self.name);
                return None;
            };
            engine.as_mut()?.call(call)
        };
        match result {
            Ok(answered) => {
                self.drain_queue();
                Some(answered)
            }
            Err(failure) => {
                let failure = match failure {
                    Failure::Trapped(why) => Failure::Trapped(self.last_error.borrow_mut().take().unwrap_or(why)),
                    other => other,
                };
                self.stop(&failure.sentence(&self.name));
                None
            }
        }
    }

    fn dispatch(&self, event: Event) -> bool {
        if self.stopped.get() {
            return false;
        }
        if self.engine.try_borrow_mut().is_err() {
            // The guest is in a call: a write it made set this off. A key or a wheel
            // needs its answer now and cannot wait, so it goes unconsumed; anything
            // else waits for the call to return.
            if !matches!(event.data, EventData::Key(_) | EventData::Wheel(_)) {
                self.queued.borrow_mut().push_back(event);
            }
            return false;
        }
        let bytes = encode(&event);
        matches!(self.call(Call::Dispatch(&bytes)), Some(Answered::Flag(true)))
    }

    fn drain_queue(&self) {
        loop {
            let next = self.queued.borrow_mut().pop_front();
            let Some(event) = next else { break };
            self.dispatch(event);
        }
    }

    /// Stop by itself: everything it built comes down, and the app hears why.
    fn stop(&self, sentence: &str) {
        tracing::warn!("{sentence}");
        self.halt();
        if let Some(f) = self.on_stop.borrow().as_ref() {
            f(sentence);
        }
    }

    fn halt(&self) {
        self.stopped.set(true);
        let engine = self.engine.borrow_mut().take();
        drop(engine);
        self.queued.borrow_mut().clear();
        self.applier.borrow_mut().clear();
        HOSTS.with(|h| h.borrow_mut().remove(&self.key));
    }

    /// Run `call` on the UI thread after `delay_ms`, if the instance still runs.
    fn later(&self, delay_ms: u32, call: impl FnOnce(&Inner) + 'static) {
        let me = self.me.clone();
        // Unowned: a timer armed while some app scope runs must not be cancelled
        // when that scope re-runs; the instance stopping is what ends it.
        rinch_core::reactive::unowned(|| {
            rinch_core::set_timeout(delay_ms, move || {
                if let Some(inner) = me.upgrade() {
                    call(&inner);
                }
            })
        });
    }
}

impl HostCalls for Inner {
    fn flush(&self, ops: &[u8]) -> Result<(), Refusal> {
        let ops: Vec<Op> = decode(ops).map_err(|_| Refusal("it sent DOM writes the host could not read".into()))?;
        self.applier.borrow_mut().apply(ops)
    }

    fn query(&self, query: &[u8]) -> Vec<u8> {
        let answer = match decode::<Query>(query) {
            Ok(query) => self.applier.borrow().answer(&query),
            Err(_) => pimble_plugin_abi::Answer::Nothing,
        };
        encode(&answer)
    }

    fn set_timeout(&self, id: u64, delay_ms: u32) {
        self.later(delay_ms, move |inner| {
            inner.call(Call::FireTimeout(id));
        });
    }

    fn schedule_task(&self, id: u64) {
        self.later(0, move |inner| {
            inner.call(Call::PollTask(id));
        });
    }

    fn wake_polls(&self) {
        if self.polls_armed.replace(true) {
            return;
        }
        self.later(0, drain_polls);
    }

    fn log(&self, level: u32, line: &str) {
        match level {
            0 => {
                tracing::error!("plugin {}: {line}", self.name);
                *self.last_error.borrow_mut() = Some(line.lines().next().unwrap_or(line).to_string());
            }
            1 => tracing::warn!("plugin {}: {line}", self.name),
            2 => tracing::info!("plugin {}: {line}", self.name),
            _ => tracing::debug!("plugin {}: {line}", self.name),
        }
    }
}

/// Sample the guest's polled signals, and arm the next drain it asks for.
fn drain_polls(inner: &Inner) {
    inner.polls_armed.set(false);
    if let Some(Answered::Number(ms)) = inner.call(Call::DrainPolls) {
        if ms >= 0 && !inner.polls_armed.replace(true) {
            // 0 is "every frame": about sixty a second.
            let delay = if ms == 0 { 16 } else { ms.min(u32::MAX as i64) as u32 };
            inner.later(delay, drain_polls);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        HOSTS.with(|h| {
            if let Ok(mut h) = h.try_borrow_mut() {
                h.remove(&self.key);
            }
        });
    }
}
