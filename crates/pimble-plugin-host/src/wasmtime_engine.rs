//! The desktop's engine (docs/PLUGINS_CONTRACT.md section 3.2): wasmtime, a core
//! module per plugin, preview1 WASI with nothing but a clock, randomness and
//! stderr, one `Store` per instance on the UI thread, and epoch interruption so a
//! plugin that runs past its budget is stopped rather than freezing the window.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use pimble_plugin_abi::{decode, encode, exports, imports, unpack, Answer, Manifest};
use wasmtime::{Caller, Config, Engine, Instance as WasmInstance, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, Trap, TypedFunc};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use crate::applier::Refusal;
use crate::instance::{host_calls, Answered, Call, Failure, GuestEngine, HostKey};

/// How often the epoch advances. A turn's budget is counted in these.
const TICK: Duration = Duration::from_millis(10);

/// The most memory one plugin instance may grow to.
const MEMORY_LIMIT: usize = 256 << 20;

/// The one engine: compiled code is shared, and one thread advances its epoch.
fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::new();
        config.epoch_interruption(true);
        let engine = Engine::new(&config).expect("wasmtime starts with epoch interruption");
        let ticking = engine.clone();
        std::thread::Builder::new()
            .name("pimble-plugin-epoch".into())
            .spawn(move || loop {
                std::thread::sleep(TICK);
                ticking.increment_epoch();
            })
            .expect("the plugin epoch thread starts");
        engine
    })
}

struct StoreData {
    wasi: WasiP1Ctx,
    /// The instance whose `HostCalls` the imports reach; 0 while describing.
    key: HostKey,
    /// The last query's answer, until the guest takes it.
    answer: Vec<u8>,
    limits: StoreLimits,
}

thread_local! {
    /// The imports, made once per thread (the UI thread, in the app).
    static LINKER: Linker<StoreData> = make_linker();
}

fn memory(caller: &mut Caller<'_, StoreData>) -> wasmtime::Result<Memory> {
    caller
        .get_export(exports::MEMORY)
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::Error::new(Refusal("it has no memory export".into())))
}

fn read(caller: &mut Caller<'_, StoreData>, ptr: u32, len: u32) -> wasmtime::Result<Vec<u8>> {
    let memory = memory(caller)?;
    let mut bytes = vec![0u8; len as usize];
    memory
        .read(&*caller, ptr as usize, &mut bytes)
        .map_err(|_| wasmtime::Error::new(Refusal("it pointed the host outside its memory".into())))?;
    Ok(bytes)
}

fn make_linker() -> Linker<StoreData> {
    let mut linker: Linker<StoreData> = Linker::new(engine());
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |d: &mut StoreData| &mut d.wasi).expect("preview1 links");
    linker
        .func_wrap(imports::MODULE, imports::FLUSH, |mut caller: Caller<'_, StoreData>, ptr: u32, len: u32| {
            let ops = read(&mut caller, ptr, len)?;
            let key = caller.data().key;
            match host_calls(key) {
                Some(host) => host.flush(&ops).map_err(wasmtime::Error::new),
                None => Err(wasmtime::Error::new(Refusal("it wrote to the window before it was shown".into()))),
            }
        })
        .expect("flush links");
    linker
        .func_wrap(imports::MODULE, imports::QUERY, |mut caller: Caller<'_, StoreData>, ptr: u32, len: u32| {
            let query = read(&mut caller, ptr, len)?;
            let answer = match host_calls(caller.data().key) {
                Some(host) => host.query(&query),
                None => encode(&Answer::Nothing),
            };
            let len = answer.len() as u32;
            caller.data_mut().answer = answer;
            Ok(len)
        })
        .expect("query links");
    linker
        .func_wrap(imports::MODULE, imports::TAKE_ANSWER, |mut caller: Caller<'_, StoreData>, ptr: u32| {
            let answer = std::mem::take(&mut caller.data_mut().answer);
            let memory = memory(&mut caller)?;
            memory
                .write(&mut caller, ptr as usize, &answer)
                .map_err(|_| wasmtime::Error::new(Refusal("it pointed the host outside its memory".into())))
        })
        .expect("take_answer links");
    linker
        .func_wrap(imports::MODULE, imports::SET_TIMEOUT, |caller: Caller<'_, StoreData>, id: u64, delay_ms: u32| {
            if let Some(host) = host_calls(caller.data().key) {
                host.set_timeout(id, delay_ms);
            }
        })
        .expect("set_timeout links");
    linker
        .func_wrap(imports::MODULE, imports::SCHEDULE_TASK, |caller: Caller<'_, StoreData>, id: u64| {
            if let Some(host) = host_calls(caller.data().key) {
                host.schedule_task(id);
            }
        })
        .expect("schedule_task links");
    linker
        .func_wrap(imports::MODULE, imports::WAKE_POLLS, |caller: Caller<'_, StoreData>| {
            if let Some(host) = host_calls(caller.data().key) {
                host.wake_polls();
            }
        })
        .expect("wake_polls links");
    linker
        .func_wrap(imports::MODULE, imports::LOG, |mut caller: Caller<'_, StoreData>, level: u32, ptr: u32, len: u32| {
            let line = read(&mut caller, ptr, len.min(64 << 10))?;
            let line = String::from_utf8_lossy(&line);
            match host_calls(caller.data().key) {
                Some(host) => host.log(level, &line),
                None => tracing::info!("plugin (loading): {line}"),
            }
            Ok(())
        })
        .expect("log links");
    linker
        .func_wrap(imports::MODULE, imports::API_CALL, |mut caller: Caller<'_, StoreData>, call: u64, ptr: u32, len: u32| {
            // A call is at most a blob's size and change; anything larger is
            // not a call this host makes.
            if len > 64 << 20 {
                return Err(wasmtime::Error::new(Refusal("it made an API call larger than any the host takes".into())));
            }
            let bytes = read(&mut caller, ptr, len)?;
            match host_calls(caller.data().key) {
                Some(host) => host.api_call(call, &bytes),
                None => tracing::info!("plugin (loading): an API call before it was shown was dropped"),
            }
            Ok(())
        })
        .expect("api_call links");
    linker
        .func_wrap(imports::MODULE, imports::API_CANCEL, |caller: Caller<'_, StoreData>, call: u64| {
            if let Some(host) = host_calls(caller.data().key) {
                host.api_cancel(call);
            }
        })
        .expect("api_cancel links");
    linker
        .func_wrap(imports::MODULE, imports::HOST, |mut caller: Caller<'_, StoreData>, ptr: u32, len: u32| {
            // A request names a node or a person: small.
            if len > 64 << 10 {
                return Err(wasmtime::Error::new(Refusal("it asked the host for something larger than any request".into())));
            }
            let bytes = read(&mut caller, ptr, len)?;
            match host_calls(caller.data().key) {
                Some(host) => host.host_request(&bytes),
                None => tracing::info!("plugin (loading): a request before it was shown was dropped"),
            }
            Ok(())
        })
        .expect("host links");
    linker
}

/// What a failed call means for the plugin.
fn failure(err: wasmtime::Error) -> Failure {
    if let Some(refusal) = err.downcast_ref::<Refusal>() {
        return Failure::Refused(refusal.0.clone());
    }
    if let Some(trap) = err.downcast_ref::<Trap>() {
        if *trap == Trap::Interrupt {
            return Failure::TooLong;
        }
        return Failure::Trapped(format!("it trapped ({trap})"));
    }
    if let Some(exit) = err.downcast_ref::<wasmtime_wasi::I32Exit>() {
        return Failure::Trapped(format!("it exited with status {}", exit.0));
    }
    Failure::Trapped(err.to_string().lines().next().unwrap_or("it failed").to_string())
}

/// A loaded plugin: its compiled module and what it said it is.
#[derive(Clone)]
pub struct Plugin {
    pub manifest: Manifest,
    pub path: PathBuf,
    module: Module,
}

/// A running wasmtime instance of a plugin.
pub struct WasmGuest {
    store: Store<StoreData>,
    memory: Memory,
    alloc: TypedFunc<u32, u32>,
    free: TypedFunc<(u32, u32), ()>,
    describe: TypedFunc<(), u64>,
    mount: TypedFunc<(u32, u32), ()>,
    dispatch: TypedFunc<(u32, u32), u32>,
    props_changed: TypedFunc<(u32, u32), ()>,
    unmount: TypedFunc<(), ()>,
    fire_timeout: TypedFunc<u64, ()>,
    poll_task: TypedFunc<u64, ()>,
    drain_polls: TypedFunc<(), i64>,
    resolve: TypedFunc<(u32, u32), ()>,
    host_state: TypedFunc<(u32, u32), ()>,
    budget_ticks: u64,
}

fn export<P: wasmtime::WasmParams, R: wasmtime::WasmResults>(
    store: &mut Store<StoreData>,
    instance: &WasmInstance,
    name: &str,
) -> Result<TypedFunc<P, R>, Failure> {
    instance
        .get_typed_func::<P, R>(&mut *store, name)
        .map_err(|_| Failure::Trapped(format!("it is not a Pimble plugin: it has no {name} export")))
}

impl WasmGuest {
    /// Instantiate `module` for the instance `key`, with `budget` for every call.
    fn new(module: &Module, key: HostKey, budget: Duration) -> Result<WasmGuest, Failure> {
        // A plugin's WASI: a clock and randomness, its stderr into the app's, no
        // files, no environment, no arguments, no network.
        let wasi = WasiCtxBuilder::new().inherit_stderr().build_p1();
        let limits = StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).instances(1).build();
        let mut store = Store::new(engine(), StoreData { wasi, key, answer: Vec::new(), limits });
        store.limiter(|d| &mut d.limits);
        store.epoch_deadline_trap();
        let budget_ticks = (budget.as_millis() / TICK.as_millis()).max(1) as u64;
        store.set_epoch_deadline(budget_ticks);
        let instance = LINKER.with(|linker| linker.instantiate(&mut store, module)).map_err(failure)?;
        if let Ok(init) = instance.get_typed_func::<(), ()>(&mut store, exports::INITIALIZE) {
            init.call(&mut store, ()).map_err(failure)?;
        }
        let memory = instance
            .get_memory(&mut store, exports::MEMORY)
            .ok_or_else(|| Failure::Trapped("it is not a Pimble plugin: it exports no memory".into()))?;
        Ok(WasmGuest {
            alloc: export(&mut store, &instance, exports::ALLOC)?,
            free: export(&mut store, &instance, exports::FREE)?,
            describe: export(&mut store, &instance, exports::DESCRIBE)?,
            mount: export(&mut store, &instance, exports::MOUNT)?,
            dispatch: export(&mut store, &instance, exports::DISPATCH)?,
            props_changed: export(&mut store, &instance, exports::PROPS_CHANGED)?,
            unmount: export(&mut store, &instance, exports::UNMOUNT)?,
            fire_timeout: export(&mut store, &instance, exports::FIRE_TIMEOUT)?,
            poll_task: export(&mut store, &instance, exports::POLL_TASK)?,
            drain_polls: export(&mut store, &instance, exports::DRAIN_POLLS)?,
            resolve: export(&mut store, &instance, exports::RESOLVE)?,
            host_state: export(&mut store, &instance, exports::HOST_STATE)?,
            memory,
            store,
            budget_ticks,
        })
    }

    /// Put `bytes` into guest memory, run `f` with where they are, and free them.
    fn with_input<R>(
        &mut self,
        bytes: &[u8],
        f: impl FnOnce(&mut Self, u32, u32) -> wasmtime::Result<R>,
    ) -> wasmtime::Result<R> {
        let len = bytes.len() as u32;
        let ptr = self.alloc.call(&mut self.store, len)?;
        self.memory
            .write(&mut self.store, ptr as usize, bytes)
            .map_err(|_| wasmtime::Error::new(Refusal("it gave the host a buffer outside its memory".into())))?;
        let result = f(self, ptr, len);
        if result.is_ok() {
            self.free.call(&mut self.store, (ptr, len))?;
        }
        result
    }

    fn take_bytes(&mut self, packed: u64) -> wasmtime::Result<Vec<u8>> {
        let (ptr, len) = unpack(packed);
        let mut bytes = vec![0u8; len as usize];
        self.memory
            .read(&self.store, ptr as usize, &mut bytes)
            .map_err(|_| wasmtime::Error::new(Refusal("it answered with a buffer outside its memory".into())))?;
        self.free.call(&mut self.store, (ptr, len))?;
        Ok(bytes)
    }

    fn run(&mut self, call: Call<'_>) -> wasmtime::Result<Answered> {
        Ok(match call {
            Call::Describe => {
                let packed = self.describe.call(&mut self.store, ())?;
                Answered::Bytes(self.take_bytes(packed)?)
            }
            Call::Mount(bytes) => {
                self.with_input(bytes, |g, p, l| g.mount.call(&mut g.store, (p, l)))?;
                Answered::Done
            }
            Call::Dispatch(bytes) => {
                let consumed = self.with_input(bytes, |g, p, l| g.dispatch.call(&mut g.store, (p, l)))?;
                Answered::Flag(consumed != 0)
            }
            Call::PropsChanged(bytes) => {
                self.with_input(bytes, |g, p, l| g.props_changed.call(&mut g.store, (p, l)))?;
                Answered::Done
            }
            Call::Unmount => {
                self.unmount.call(&mut self.store, ())?;
                Answered::Done
            }
            Call::FireTimeout(id) => {
                self.fire_timeout.call(&mut self.store, id)?;
                Answered::Done
            }
            Call::PollTask(id) => {
                self.poll_task.call(&mut self.store, id)?;
                Answered::Done
            }
            Call::DrainPolls => Answered::Number(self.drain_polls.call(&mut self.store, ())?),
            Call::Resolve(bytes) => {
                self.with_input(bytes, |g, p, l| g.resolve.call(&mut g.store, (p, l)))?;
                Answered::Done
            }
            Call::HostState(bytes) => {
                self.with_input(bytes, |g, p, l| g.host_state.call(&mut g.store, (p, l)))?;
                Answered::Done
            }
        })
    }
}

impl GuestEngine for WasmGuest {
    fn call(&mut self, call: Call<'_>) -> Result<Answered, Failure> {
        // Every turn starts with the whole budget: the deadline is counted from now.
        self.store.set_epoch_deadline(self.budget_ticks);
        self.run(call).map_err(failure)
    }
}

/// How long one guest turn may run before the plugin is stopped.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(2);

/// Compile and describe the plugin at `path`, refusing one built for another ABI.
/// The error is the sentence a person reads, with the manifest when there was one
/// (so a refused plugin's viewers can say why they are missing).
pub fn load_file(path: &Path, budget: Duration) -> Result<Plugin, (Option<Manifest>, String)> {
    let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let bytes = std::fs::read(path).map_err(|e| (None, format!("The plugin {file} could not be read: {e}.")))?;
    load_bytes(path, &bytes, budget)
}

/// [`load_file`] for a module already in memory.
pub fn load_bytes(path: &Path, bytes: &[u8], budget: Duration) -> Result<Plugin, (Option<Manifest>, String)> {
    let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let module = Module::new(engine(), bytes)
        .map_err(|e| (None, format!("The plugin {file} is not a WebAssembly module Pimble can run: {}.", first_line(&e))))?;
    let mut guest = WasmGuest::new(&module, 0, budget).map_err(|f| (None, f.sentence(&file)))?;
    let bytes = match guest.call(Call::Describe) {
        Ok(Answered::Bytes(bytes)) => bytes,
        Ok(_) => return Err((None, format!("The plugin {file} did not describe itself."))),
        Err(f) => return Err((None, f.sentence(&file))),
    };
    let manifest: Manifest =
        decode(&bytes).map_err(|_| (None, format!("The plugin {file} described itself in a form this Pimble cannot read.")))?;
    if let Err(sentence) = crate::check_manifest(&manifest) {
        return Err((Some(manifest), sentence));
    }
    Ok(Plugin { manifest, path: path.to_path_buf(), module })
}

fn first_line(e: &wasmtime::Error) -> String {
    e.to_string().lines().next().unwrap_or_default().to_string()
}

impl Plugin {
    /// An engine running a fresh instance of this plugin, for `key`.
    pub fn instantiate(&self, key: HostKey, budget: Duration) -> Result<Box<dyn GuestEngine>, Failure> {
        Ok(Box::new(WasmGuest::new(&self.module, key, budget)?))
    }
}

/// The plugins in a folder, and the ones refused.
#[derive(Clone, Default)]
pub struct Registry {
    pub plugins: Vec<Plugin>,
    /// Files that are not plugins this app can run, each with why, and the viewers
    /// it said it provides when it could say.
    pub refused: Vec<Refused>,
}

/// A plugin file the app does not run.
#[derive(Debug, Clone)]
pub struct Refused {
    pub path: PathBuf,
    pub viewers: Vec<String>,
    pub sentence: String,
}

/// What the app has for a viewer id.
pub enum ViewerLookup<'a> {
    Ready(&'a Plugin),
    /// A plugin provides it but was refused; the sentence says why.
    Refused(&'a str),
    Missing,
}

impl Registry {
    /// Every `.wasm` file in `dir`, loaded. A folder that does not exist holds none.
    pub fn load(dir: &Path, budget: Duration) -> Registry {
        let mut registry = Registry::default();
        let Ok(entries) = std::fs::read_dir(dir) else { return registry };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "wasm"))
            .collect();
        paths.sort();
        for path in paths {
            match load_file(&path, budget) {
                Ok(plugin) => {
                    if plugin.manifest.rinch != crate::HOST_RINCH_REVISION {
                        tracing::info!(
                            "plugin {} was built against rinch {}; this app runs rinch {}",
                            plugin.manifest.id,
                            plugin.manifest.rinch,
                            crate::HOST_RINCH_REVISION
                        );
                    }
                    tracing::info!("plugin {} {} loaded from {}", plugin.manifest.id, plugin.manifest.version, path.display());
                    registry.plugins.push(plugin);
                }
                Err((manifest, sentence)) => {
                    tracing::warn!("{sentence}");
                    let viewers = manifest.map(|m| m.viewers().map(|v| v.id.clone()).collect()).unwrap_or_default();
                    registry.refused.push(Refused { path, viewers, sentence });
                }
            }
        }
        registry
    }

    /// The plugin providing viewer `id`, or why there is none to run.
    pub fn viewer(&self, id: &str) -> ViewerLookup<'_> {
        if let Some(plugin) = self.plugins.iter().find(|p| p.manifest.viewers().any(|v| v.id == id)) {
            return ViewerLookup::Ready(plugin);
        }
        match self.refused.iter().find(|r| r.viewers.iter().any(|v| v == id)) {
            Some(refused) => ViewerLookup::Refused(&refused.sentence),
            None => ViewerLookup::Missing,
        }
    }
}
