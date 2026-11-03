//! The wasm side of the ABI: the host's imports, the hooks that run rinch's timers,
//! tasks and polls through them, and what the exports [`crate::plugin!`] defines call.

use std::alloc::{alloc as raw_alloc, dealloc, Layout};
use std::cell::RefCell;
use std::rc::Rc;

use pimble_plugin_abi::{decode, encode, pack, Answer, Event, Manifest, MountArgs, Op, Props, Query};

use crate::document::HostLink;
use crate::runtime::{Guest, ViewFn};

#[link(wasm_import_module = "pimble")]
unsafe extern "C" {
    fn pimble_flush(ptr: *const u8, len: u32);
    fn pimble_query(ptr: *const u8, len: u32) -> u32;
    fn pimble_take_answer(ptr: *mut u8);
    fn pimble_set_timeout(id: u64, delay_ms: u32);
    fn pimble_schedule_task(id: u64);
    fn pimble_wake_polls();
    fn pimble_log(level: u32, ptr: *const u8, len: u32);
}

/// The host, through the imports.
struct WasmHost;

impl HostLink for WasmHost {
    fn flush(&self, ops: Vec<Op>) {
        let bytes = encode(&ops);
        unsafe { pimble_flush(bytes.as_ptr(), bytes.len() as u32) }
    }

    fn query(&self, query: &Query) -> Answer {
        let bytes = encode(query);
        let len = unsafe { pimble_query(bytes.as_ptr(), bytes.len() as u32) } as usize;
        let mut answer = vec![0u8; len];
        unsafe { pimble_take_answer(answer.as_mut_ptr()) }
        decode(&answer).unwrap_or(Answer::Nothing)
    }
}

pub(crate) fn log(level: u32, line: &str) {
    unsafe { pimble_log(level, line.as_ptr(), line.len() as u32) }
}

fn set_timeout_hook(id: u64, delay_ms: u32) {
    unsafe { pimble_set_timeout(id, delay_ms) }
}

fn schedule_task_hook(id: u64) {
    unsafe { pimble_schedule_task(id) }
}

fn wake_polls_hook() {
    unsafe { pimble_wake_polls() }
}

thread_local! {
    static GUEST: RefCell<Option<Guest>> = const { RefCell::new(None) };
}

/// The guest, made on first use with rinch's runtime hooks pointed at the host:
/// the host has no rinch frame loop to lend a guest, so timers, tasks and polled
/// signals run when the host calls the matching exports, as the browser runtime
/// runs them off `setTimeout` and microtasks.
fn with_guest<R>(f: impl FnOnce(&mut Guest) -> R) -> R {
    GUEST.with(|cell| {
        let mut slot = cell.borrow_mut();
        let guest = slot.get_or_insert_with(|| {
            rinch::core::set_timer_backend(set_timeout_hook);
            rinch::core::task::set_task_scheduler(schedule_task_hook);
            rinch::core::reactive::set_poll_waker(wake_polls_hook);
            std::panic::set_hook(Box::new(|info| log(0, &info.to_string())));
            Guest::new(Rc::new(WasmHost))
        });
        f(guest)
    })
}

fn input<'a>(ptr: u32, len: u32) -> &'a [u8] {
    if len == 0 {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) }
}

/// Bytes for the host, in memory it frees with `pimble_free`.
fn answer(bytes: &[u8]) -> u64 {
    let ptr = alloc(bytes.len() as u32);
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    pack(ptr, bytes.len() as u32)
}

pub fn alloc(len: u32) -> u32 {
    let layout = Layout::from_size_align(len.max(1) as usize, 8).expect("a buffer layout");
    unsafe { raw_alloc(layout) as u32 }
}

pub fn free(ptr: u32, len: u32) {
    if ptr == 0 {
        return;
    }
    let layout = Layout::from_size_align(len.max(1) as usize, 8).expect("a buffer layout");
    unsafe { dealloc(ptr as *mut u8, layout) }
}

pub fn describe(describe: fn() -> Manifest) -> u64 {
    answer(&encode(&describe()))
}

pub fn mount(view: ViewFn, ptr: u32, len: u32) {
    let args: MountArgs = decode(input(ptr, len)).expect("the host sends mount arguments");
    with_guest(|g| g.mount(view, args));
}

pub fn dispatch(ptr: u32, len: u32) -> u32 {
    let event: Event = decode(input(ptr, len)).expect("the host sends an event");
    with_guest(|g| g.dispatch(event)) as u32
}

pub fn props_changed(ptr: u32, len: u32) {
    let props: Props = decode(input(ptr, len)).expect("the host sends props");
    with_guest(|g| g.props_changed(props));
}

pub fn unmount() {
    with_guest(|g| g.unmount());
}

pub fn fire_timeout(id: u64) {
    rinch::core::fire_timeout(id);
    with_guest(|g| g.flush());
}

pub fn poll_task(id: u64) {
    rinch::core::task::poll_task(id);
    with_guest(|g| g.flush());
}

pub fn drain_polls() -> i64 {
    rinch::core::batch(rinch::core::reactive::drain_polls);
    with_guest(|g| g.flush());
    if rinch::core::reactive::polls_want_every_frame() {
        0
    } else {
        rinch::core::reactive::next_poll_due().map_or(-1, |due| due.as_millis().max(1) as i64)
    }
}
