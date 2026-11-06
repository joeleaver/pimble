//! The browser's engine (docs/PLUGINS_CONTRACT.md section 3.2, wave 3): the same
//! `wasm32-wasip1` core modules the desktop runs under wasmtime, run by the page's
//! own `WebAssembly`.
//!
//! - **Imports** are an object of wasm-bindgen closures that mirror the wasmtime
//!   linker's one for one: each reads and writes the guest's memory through a
//!   fresh `Uint8Array` view of its buffer (a view goes stale when the memory
//!   grows), and reaches the instance's [`HostCalls`](crate::instance::HostCalls)
//!   through its key, exactly as the desktop's do.
//! - **WASI** is a shim of what a plugin uses: `fd_write` into the log, the clock,
//!   `crypto.getRandomValues`, an empty environment and argument list,
//!   `proc_exit`, `sched_yield`. Any other preview1 function a module imports
//!   answers "not supported" (`ENOSYS`), as a plugin has no files, sockets or
//!   anything else to reach.
//! - **Stopping.** An import that refuses what the guest sent (a node not its own)
//!   records why and throws; the throw unwinds the guest to the export call, which
//!   reads the reason back. A browser cannot interrupt running code, so a turn is
//!   timed instead, and one that ran past its budget stops the plugin when it
//!   returns, with the desktop's notice (decision 5). A plugin that never returns
//!   holds the page until the browser offers to stop it.
//! - **Ready ahead.** Compiling and instantiating are asynchronous here (a browser
//!   may refuse to do either synchronously for a module this size on the page's
//!   thread), while a pane mounts a plugin synchronously. So each plugin keeps one
//!   instance ready, made when it loads and again each time one is taken; a pane
//!   that finds none ready shows the outline until it is.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use js_sys::{Array, Function, Object, Reflect, Uint8Array, WebAssembly};
use pimble_plugin_abi::{decode, encode, exports, imports, unpack, Answer, Manifest};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::applier::Refusal;
use crate::instance::{host_calls, Answered, Call, Failure, GuestEngine, HostKey};

/// How long one guest turn may run before the plugin is stopped when it returns:
/// the desktop's budget, measured instead of enforced.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(2);

const WASI: &str = "wasi_snapshot_preview1";
/// preview1's "function not supported".
const ENOSYS: i32 = 52;
/// preview1's "bad address": the guest pointed outside its memory.
const EFAULT: i32 = 21;

/// What an instance's imports share with the engine that calls its exports.
#[derive(Default)]
struct Shared {
    /// The instance whose `HostCalls` the imports reach; 0 while describing or
    /// while the instance waits to be taken.
    key: Cell<HostKey>,
    memory: RefCell<Option<WebAssembly::Memory>>,
    /// The last query's answer, until the guest takes it.
    answer: RefCell<Vec<u8>>,
    /// Why an import stopped the guest, for the export call that sees it throw.
    failure: RefCell<Option<Failure>>,
}

fn outside() -> Refusal {
    Refusal("it pointed the host outside its memory".into())
}

impl Shared {
    /// The guest's memory as it is now.
    fn view(&self) -> Result<Uint8Array, Refusal> {
        let memory = self.memory.borrow();
        let memory = memory.as_ref().ok_or_else(|| Refusal("it has no memory export".into()))?;
        Ok(Uint8Array::new(&memory.buffer()))
    }

    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, Refusal> {
        let view = self.view()?;
        if ptr as u64 + len as u64 > view.length() as u64 {
            return Err(outside());
        }
        Ok(view.subarray(ptr, ptr + len).to_vec())
    }

    fn write(&self, ptr: u32, bytes: &[u8]) -> Result<(), Refusal> {
        let view = self.view()?;
        if ptr as u64 + bytes.len() as u64 > view.length() as u64 {
            return Err(outside());
        }
        view.subarray(ptr, ptr + bytes.len() as u32).copy_from(bytes);
        Ok(())
    }

    fn read_u32(&self, ptr: u32) -> Result<u32, Refusal> {
        let bytes = self.read(ptr, 4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

/// Stop the guest from inside an import: say why, then throw, which unwinds the
/// guest to the export call. Nothing of the host may be borrowed here: the
/// frames the throw passes are abandoned, not unwound.
fn stop(shared: &Shared, failure: Failure) -> ! {
    shared.failure.replace(Some(failure));
    wasm_bindgen::throw_str("the Pimble plugin host stopped this plugin")
}

/// A guest's `i64` (a JavaScript BigInt) as the `u64` it stands for.
fn u64_of(value: JsValue) -> u64 {
    u64::try_from(value.clone()).or_else(|_| i64::try_from(value).map(|v| v as u64)).unwrap_or(0)
}

/// A guest's `i32` result (a JavaScript number, signed) as the `u32` it stands for.
fn u32_of(value: &JsValue) -> u32 {
    value.as_f64().map_or(0, |v| v as i64 as u32)
}

/// What a JavaScript error says, first line.
fn message(error: &JsValue) -> String {
    let text = match error.dyn_ref::<js_sys::Error>() {
        Some(e) => String::from(e.message()),
        None => error.as_string().unwrap_or_else(|| format!("{error:?}")),
    };
    text.lines().next().unwrap_or_default().to_string()
}

/// The closures an instance's imports are made of. Kept as long as the instance:
/// a closure dropped while the guest can still call it would throw.
type Kept = Vec<Box<dyn Any>>;

fn set(object: &Object, name: &str, value: &JsValue) {
    let _ = Reflect::set(object, &JsValue::from_str(name), value);
}

/// Put `closure` into `object` as `name`, and keep it.
fn put<F: ?Sized + wasm_bindgen::closure::WasmClosure + 'static>(object: &Object, kept: &mut Kept, name: &str, closure: Closure<F>) {
    set(object, name, closure.as_ref());
    kept.push(Box::new(closure));
}

/// The `pimble` imports: the wasmtime linker's, one for one.
fn pimble_imports(shared: &Rc<Shared>, kept: &mut Kept) -> Object {
    let object = Object::new();
    let s = shared.clone();
    put(&object, kept, imports::FLUSH, Closure::<dyn Fn(u32, u32)>::new(move |ptr: u32, len: u32| {
        let applied = s.read(ptr, len).and_then(|ops| match host_calls(s.key.get()) {
            Some(host) => host.flush(&ops),
            None => Err(Refusal("it wrote to the window before it was shown".into())),
        });
        if let Err(refusal) = applied {
            stop(&s, Failure::Refused(refusal.0));
        }
    }));
    let s = shared.clone();
    put(&object, kept, imports::QUERY, Closure::<dyn Fn(u32, u32) -> u32>::new(move |ptr: u32, len: u32| {
        let answer = match s.read(ptr, len) {
            Ok(query) => match host_calls(s.key.get()) {
                Some(host) => host.query(&query),
                None => encode(&Answer::Nothing),
            },
            Err(refusal) => stop(&s, Failure::Refused(refusal.0)),
        };
        let len = answer.len() as u32;
        s.answer.replace(answer);
        len
    }));
    let s = shared.clone();
    put(&object, kept, imports::TAKE_ANSWER, Closure::<dyn Fn(u32)>::new(move |ptr: u32| {
        let answer = s.answer.take();
        if let Err(refusal) = s.write(ptr, &answer) {
            stop(&s, Failure::Refused(refusal.0));
        }
    }));
    let s = shared.clone();
    put(&object, kept, imports::SET_TIMEOUT, Closure::<dyn Fn(JsValue, u32)>::new(move |id: JsValue, delay_ms: u32| {
        if let Some(host) = host_calls(s.key.get()) {
            host.set_timeout(u64_of(id), delay_ms);
        }
    }));
    let s = shared.clone();
    put(&object, kept, imports::SCHEDULE_TASK, Closure::<dyn Fn(JsValue)>::new(move |id: JsValue| {
        if let Some(host) = host_calls(s.key.get()) {
            host.schedule_task(u64_of(id));
        }
    }));
    let s = shared.clone();
    put(&object, kept, imports::WAKE_POLLS, Closure::<dyn Fn()>::new(move || {
        if let Some(host) = host_calls(s.key.get()) {
            host.wake_polls();
        }
    }));
    let s = shared.clone();
    put(&object, kept, imports::LOG, Closure::<dyn Fn(u32, u32, u32)>::new(move |level: u32, ptr: u32, len: u32| {
        let line = match s.read(ptr, len.min(64 << 10)) {
            Ok(line) => line,
            Err(refusal) => stop(&s, Failure::Refused(refusal.0)),
        };
        let line = String::from_utf8_lossy(&line);
        match host_calls(s.key.get()) {
            Some(host) => host.log(level, &line),
            None => tracing::info!("plugin (loading): {line}"),
        }
    }));
    let s = shared.clone();
    put(&object, kept, imports::API_CALL, Closure::<dyn Fn(JsValue, u32, u32)>::new(move |call: JsValue, ptr: u32, len: u32| {
        // A call is at most a blob's size and change; anything larger is not a
        // call this host makes.
        if len > 64 << 20 {
            stop(&s, Failure::Refused("it made an API call larger than any the host takes".into()));
        }
        let bytes = match s.read(ptr, len) {
            Ok(bytes) => bytes,
            Err(refusal) => stop(&s, Failure::Refused(refusal.0)),
        };
        match host_calls(s.key.get()) {
            Some(host) => host.api_call(u64_of(call), &bytes),
            None => tracing::info!("plugin (loading): an API call before it was shown was dropped"),
        }
    }));
    let s = shared.clone();
    put(&object, kept, imports::API_CANCEL, Closure::<dyn Fn(JsValue)>::new(move |call: JsValue| {
        if let Some(host) = host_calls(s.key.get()) {
            host.api_cancel(u64_of(call));
        }
    }));
    let s = shared.clone();
    put(&object, kept, imports::HOST, Closure::<dyn Fn(u32, u32)>::new(move |ptr: u32, len: u32| {
        // A request names a node or a person: small.
        if len > 64 << 10 {
            stop(&s, Failure::Refused("it asked the host for something larger than any request".into()));
        }
        let bytes = match s.read(ptr, len) {
            Ok(bytes) => bytes,
            Err(refusal) => stop(&s, Failure::Refused(refusal.0)),
        };
        match host_calls(s.key.get()) {
            Some(host) => host.host_request(&bytes),
            None => tracing::info!("plugin (loading): a request before it was shown was dropped"),
        }
    }));
    object
}

/// Fill `buf` from the browser's cryptographic randomness (a plugin's node ids
/// come from it), or from `Math.random` where there is none.
fn random_bytes(buf: &mut [u8]) {
    let crypto = Reflect::get(&js_sys::global(), &JsValue::from_str("crypto")).ok().filter(|c| c.is_object());
    let get = crypto.as_ref().and_then(|c| Reflect::get(c, &JsValue::from_str("getRandomValues")).ok()).and_then(|f| f.dyn_into::<Function>().ok());
    for chunk in buf.chunks_mut(65536) {
        let filled = match (&crypto, &get) {
            (Some(crypto), Some(get)) => {
                let array = Uint8Array::new_with_length(chunk.len() as u32);
                get.call1(crypto, &array).is_ok() && {
                    array.copy_to(chunk);
                    true
                }
            }
            _ => false,
        };
        if !filled {
            for b in chunk.iter_mut() {
                *b = (js_sys::Math::random() * 256.0) as u8;
            }
        }
    }
}

/// The preview1 shim: what a plugin built for `wasm32-wasip1` imports, and
/// "not supported" for anything else of preview1 that `module` imports.
fn wasi_imports(module: &WebAssembly::Module, shared: &Rc<Shared>, kept: &mut Kept) -> Object {
    let object = Object::new();
    let s = shared.clone();
    put(&object, kept, "fd_write", Closure::<dyn Fn(u32, u32, u32, u32) -> i32>::new(move |fd: u32, iovs: u32, count: u32, written: u32| {
        let mut out = Vec::new();
        for i in 0..count.min(1024) {
            let (Ok(buf), Ok(len)) = (s.read_u32(iovs + i * 8), s.read_u32(iovs + i * 8 + 4)) else { return EFAULT };
            match s.read(buf, len) {
                Ok(bytes) => out.extend(bytes),
                Err(_) => return EFAULT,
            }
        }
        let text = String::from_utf8_lossy(&out);
        let text = text.trim_end();
        if !text.is_empty() {
            if fd == 2 {
                tracing::warn!("plugin stderr: {text}");
            } else {
                tracing::info!("plugin stdout: {text}");
            }
        }
        match s.write(written, &(out.len() as u32).to_le_bytes()) {
            Ok(()) => 0,
            Err(_) => EFAULT,
        }
    }));
    let s = shared.clone();
    put(&object, kept, "clock_time_get", Closure::<dyn Fn(u32, JsValue, u32) -> i32>::new(move |_clock: u32, _precision: JsValue, at: u32| {
        // Both clocks from the wall clock, in nanoseconds: a plugin times
        // nothing finer than a frame.
        let nanos = (js_sys::Date::now() * 1_000_000.0) as u64;
        match s.write(at, &nanos.to_le_bytes()) {
            Ok(()) => 0,
            Err(_) => EFAULT,
        }
    }));
    let s = shared.clone();
    put(&object, kept, "random_get", Closure::<dyn Fn(u32, u32) -> i32>::new(move |buf: u32, len: u32| {
        let mut bytes = vec![0u8; len as usize];
        random_bytes(&mut bytes);
        match s.write(buf, &bytes) {
            Ok(()) => 0,
            Err(_) => EFAULT,
        }
    }));
    // No environment and no arguments: both counts are zero.
    for name in ["environ_sizes_get", "args_sizes_get"] {
        let s = shared.clone();
        put(&object, kept, name, Closure::<dyn Fn(u32, u32) -> i32>::new(move |count: u32, size: u32| {
            match s.write(count, &0u32.to_le_bytes()).and_then(|_| s.write(size, &0u32.to_le_bytes())) {
                Ok(()) => 0,
                Err(_) => EFAULT,
            }
        }));
    }
    for name in ["environ_get", "args_get"] {
        put(&object, kept, name, Closure::<dyn Fn(u32, u32) -> i32>::new(|_: u32, _: u32| 0));
    }
    put(&object, kept, "sched_yield", Closure::<dyn Fn() -> i32>::new(|| 0));
    let s = shared.clone();
    put(&object, kept, "proc_exit", Closure::<dyn Fn(u32)>::new(move |code: u32| {
        stop(&s, Failure::Trapped(format!("it exited with status {code}")));
    }));
    // Whatever else of preview1 the module imports: it has nothing to reach.
    for import in WebAssembly::Module::imports(module).iter() {
        let module_name = Reflect::get(&import, &JsValue::from_str("module")).ok().and_then(|v| v.as_string());
        let name = Reflect::get(&import, &JsValue::from_str("name")).ok().and_then(|v| v.as_string());
        let (Some(module_name), Some(name)) = (module_name, name) else { continue };
        if module_name == WASI && !Reflect::has(&object, &JsValue::from_str(&name)).unwrap_or(false) {
            put(&object, kept, &name, Closure::<dyn Fn() -> i32>::new(|| ENOSYS));
        }
    }
    object
}

/// A running instance of a plugin in the page.
pub struct JsGuest {
    shared: Rc<Shared>,
    _kept: Kept,
    alloc: Function,
    free: Function,
    describe: Function,
    mount: Function,
    dispatch: Function,
    props_changed: Function,
    unmount: Function,
    fire_timeout: Function,
    poll_task: Function,
    drain_polls: Function,
    resolve: Function,
    host_state: Function,
    drag_ended: Function,
    budget_ms: f64,
}

impl Drop for JsGuest {
    fn drop(&mut self) {
        // Let go of the memory, so nothing a stray closure still reaches keeps it.
        self.shared.memory.replace(None);
    }
}

impl JsGuest {
    /// Instantiate `module` for no instance yet (key 0), with `budget` for every
    /// call.
    async fn new(module: &WebAssembly::Module, budget: Duration) -> Result<JsGuest, Failure> {
        let shared = Rc::new(Shared::default());
        let mut kept = Kept::new();
        let imports = Object::new();
        set(&imports, imports::MODULE, &pimble_imports(&shared, &mut kept));
        set(&imports, WASI, &wasi_imports(module, &shared, &mut kept));
        let instance = JsFuture::from(WebAssembly::instantiate_module(module, &imports))
            .await
            .map_err(|e| Failure::Trapped(format!("it could not be started: {}", message(&e))))?;
        let instance: WebAssembly::Instance = instance.dyn_into().map_err(|_| Failure::Trapped("it could not be started".into()))?;
        let exported = instance.exports();
        let get = |name: &str| Reflect::get(&exported, &JsValue::from_str(name)).ok().filter(|v| !v.is_undefined());
        let func = |name: &str| {
            get(name)
                .and_then(|f| f.dyn_into::<Function>().ok())
                .ok_or_else(|| Failure::Trapped(format!("it is not a Pimble plugin: it has no {name} export")))
        };
        let memory = get(exports::MEMORY)
            .and_then(|m| m.dyn_into::<WebAssembly::Memory>().ok())
            .ok_or_else(|| Failure::Trapped("it is not a Pimble plugin: it exports no memory".into()))?;
        shared.memory.replace(Some(memory));
        let guest = JsGuest {
            alloc: func(exports::ALLOC)?,
            free: func(exports::FREE)?,
            describe: func(exports::DESCRIBE)?,
            mount: func(exports::MOUNT)?,
            dispatch: func(exports::DISPATCH)?,
            props_changed: func(exports::PROPS_CHANGED)?,
            unmount: func(exports::UNMOUNT)?,
            fire_timeout: func(exports::FIRE_TIMEOUT)?,
            poll_task: func(exports::POLL_TASK)?,
            drain_polls: func(exports::DRAIN_POLLS)?,
            resolve: func(exports::RESOLVE)?,
            host_state: func(exports::HOST_STATE)?,
            drag_ended: func(exports::DRAG_ENDED)?,
            shared,
            _kept: kept,
            budget_ms: budget.as_secs_f64() * 1000.0,
        };
        if let Ok(init) = func(exports::INITIALIZE) {
            guest.timed(|g| g.invoke(&init, &[]))?;
        }
        Ok(guest)
    }

    /// Call export `f` with `args`; a throw is the failure an import recorded, or a
    /// trap.
    fn invoke(&self, f: &Function, args: &[JsValue]) -> Result<JsValue, Failure> {
        let this = JsValue::UNDEFINED;
        let result = match args {
            [] => f.call0(&this),
            [a] => f.call1(&this, a),
            [a, b] => f.call2(&this, a, b),
            _ => f.apply(&this, &args.iter().cloned().collect::<Array>()),
        };
        result.map_err(|e| self.shared.failure.take().unwrap_or_else(|| Failure::Trapped(format!("it trapped ({})", message(&e)))))
    }

    /// Run one turn, and stop the plugin if it ran past its budget: a browser
    /// cannot interrupt it, so the notice comes when it returns (decision 5).
    fn timed<R>(&self, turn: impl FnOnce(&Self) -> Result<R, Failure>) -> Result<R, Failure> {
        let started = js_sys::Date::now();
        let result = turn(self);
        if result.is_ok() && js_sys::Date::now() - started > self.budget_ms {
            return Err(Failure::TooLong);
        }
        result
    }

    /// Put `bytes` into guest memory, call `f` with where they are, and free them.
    fn with_input(&self, f: &Function, bytes: &[u8]) -> Result<JsValue, Failure> {
        let len = bytes.len() as u32;
        let ptr = u32_of(&self.invoke(&self.alloc, &[JsValue::from(len)])?);
        self.shared
            .write(ptr, bytes)
            .map_err(|_| Failure::Refused("it gave the host a buffer outside its memory".into()))?;
        let result = self.invoke(f, &[JsValue::from(ptr), JsValue::from(len)])?;
        self.invoke(&self.free, &[JsValue::from(ptr), JsValue::from(len)])?;
        Ok(result)
    }

    fn take_bytes(&self, packed: u64) -> Result<Vec<u8>, Failure> {
        let (ptr, len) = unpack(packed);
        let bytes = self
            .shared
            .read(ptr, len)
            .map_err(|_| Failure::Refused("it answered with a buffer outside its memory".into()))?;
        self.invoke(&self.free, &[JsValue::from(ptr), JsValue::from(len)])?;
        Ok(bytes)
    }

    fn run(&self, call: Call<'_>) -> Result<Answered, Failure> {
        let big = |id: u64| JsValue::from(js_sys::BigInt::from(id));
        Ok(match call {
            Call::Describe => {
                let packed = u64_of(self.invoke(&self.describe, &[])?);
                Answered::Bytes(self.take_bytes(packed)?)
            }
            Call::Mount(bytes) => {
                self.with_input(&self.mount, bytes)?;
                Answered::Done
            }
            Call::Dispatch(bytes) => Answered::Dispatched(u32_of(&self.with_input(&self.dispatch, bytes)?)),
            Call::PropsChanged(bytes) => {
                self.with_input(&self.props_changed, bytes)?;
                Answered::Done
            }
            Call::Unmount => {
                self.invoke(&self.unmount, &[])?;
                Answered::Done
            }
            Call::FireTimeout(id) => {
                self.invoke(&self.fire_timeout, &[big(id)])?;
                Answered::Done
            }
            Call::PollTask(id) => {
                self.invoke(&self.poll_task, &[big(id)])?;
                Answered::Done
            }
            Call::DrainPolls => Answered::Number(u64_of(self.invoke(&self.drain_polls, &[])?) as i64),
            Call::Resolve(bytes) => {
                self.with_input(&self.resolve, bytes)?;
                Answered::Done
            }
            Call::HostState(bytes) => {
                self.with_input(&self.host_state, bytes)?;
                Answered::Done
            }
            Call::DragEnded => {
                self.invoke(&self.drag_ended, &[])?;
                Answered::Done
            }
            // Tools run for agents, in the MCP, never in a page.
            Call::Tool(_) | Call::ToolResult(_) => return Err(Failure::Refused("tools do not run in the browser".into())),
        })
    }
}

impl GuestEngine for JsGuest {
    fn call(&mut self, call: Call<'_>) -> Result<Answered, Failure> {
        self.timed(|g| g.run(call))
    }
}

/// A loaded plugin: its compiled module, what it said it is, and an instance
/// ready to be taken.
#[derive(Clone)]
pub struct Plugin {
    pub manifest: Manifest,
    /// The file it came from (the name the person's file had).
    pub path: PathBuf,
    module: WebAssembly::Module,
    budget: Duration,
    ready: Rc<RefCell<Option<JsGuest>>>,
    /// Who waits for the instance being made: empty when none is.
    warming: Rc<RefCell<Option<Vec<Box<dyn FnOnce()>>>>>,
    /// Why the last instance could not be made, so a pane says it rather than
    /// waiting for one that will not come.
    broken: Rc<RefCell<Option<Failure>>>,
}

impl crate::registry::Loaded for Plugin {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

/// The plugins the page has loaded, and the ones refused.
pub type Registry = crate::registry::Registry<Plugin>;
/// What the page has for a viewer id.
pub type ViewerLookup<'a> = crate::registry::ViewerLookup<'a, Plugin>;

impl Plugin {
    /// Whether an instance is ready to be taken now.
    pub fn is_ready(&self) -> bool {
        self.ready.borrow().is_some()
    }

    /// Whether the last instance could not be made: taking one then says why.
    pub fn is_broken(&self) -> bool {
        self.broken.borrow().is_some()
    }

    /// Make an instance ready if none is (or is being made), and call `then` once
    /// one is, or once making it failed (the failure is logged; taking one then
    /// says why).
    pub fn warm(&self, then: impl FnOnce() + 'static) {
        if self.is_ready() {
            then();
            return;
        }
        if let Some(waiting) = self.warming.borrow_mut().as_mut() {
            waiting.push(Box::new(then));
            return;
        }
        self.warming.replace(Some(vec![Box::new(then)]));
        let (module, budget, ready, warming, broken) = (self.module.clone(), self.budget, self.ready.clone(), self.warming.clone(), self.broken.clone());
        let id = self.manifest.id.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match JsGuest::new(&module, budget).await {
                Ok(guest) => {
                    ready.replace(Some(guest));
                    broken.replace(None);
                }
                Err(failure) => {
                    tracing::warn!("plugin {id}: an instance could not be made ready: {failure:?}");
                    broken.replace(Some(failure));
                }
            }
            // Taken before any is called: one may ask to warm again.
            let waiting = warming.take().unwrap_or_default();
            for then in waiting {
                then();
            }
        });
    }

    /// The ready instance, for the instance `key`; another is made ready behind
    /// it. With none ready (a second pane mounting the same plugin at once), the
    /// failure says to wait; [`Plugin::warm`] says when to try again.
    pub fn instantiate(&self, key: HostKey) -> Result<Box<dyn GuestEngine>, Failure> {
        let taken = self.ready.borrow_mut().take();
        self.warm(|| {});
        match taken {
            Some(guest) => {
                guest.shared.key.set(key);
                Ok(Box::new(guest))
            }
            None => Err(self.broken.borrow().clone().unwrap_or_else(|| Failure::Trapped("it is still starting".into()))),
        }
    }
}

/// Compile and describe a plugin the person added (`name` is its file's name),
/// refusing one built for another ABI, and make an instance ready. The error is
/// the sentence a person reads, with the manifest when there was one.
pub async fn load_bytes(name: &str, bytes: &[u8], budget: Duration) -> Result<Plugin, (Option<Manifest>, String)> {
    let path = Path::new(name).to_path_buf();
    let compiled = JsFuture::from(WebAssembly::compile(&Uint8Array::from(bytes).into())).await;
    let module: WebAssembly::Module = compiled
        .ok()
        .and_then(|m| m.dyn_into().ok())
        .ok_or_else(|| (None, format!("The plugin {name} is not a WebAssembly module Pimble can run.")))?;
    // The instance that describes it is the first one taken: describing touches
    // nothing a mount reads.
    let mut guest = JsGuest::new(&module, budget).await.map_err(|f| (None, f.sentence(name)))?;
    let described = match guest.call(Call::Describe) {
        Ok(Answered::Bytes(bytes)) => bytes,
        Ok(_) => return Err((None, format!("The plugin {name} did not describe itself."))),
        Err(f) => return Err((None, f.sentence(name))),
    };
    let manifest: Manifest =
        decode(&described).map_err(|_| (None, format!("The plugin {name} described itself in a form this Pimble cannot read.")))?;
    crate::check_manifest(&manifest).map_err(|sentence| (Some(manifest.clone()), sentence))?;
    Ok(Plugin { manifest, path, module, budget, ready: Rc::new(RefCell::new(Some(guest))), warming: Rc::default(), broken: Rc::default() })
}
