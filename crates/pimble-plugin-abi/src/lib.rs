//! The plugin ABI (docs/PLUGINS_CONTRACT.md section 3): what crosses between a Pimble
//! plugin and the app that runs it.
//!
//! A plugin is a `wasm32-wasip1` core module. It renders plain rinch into a document
//! of its own (the guest SDK's `RemoteDocument`) and sends the host the DOM writes it
//! made as an ordered list of [`Op`]s; the host applies them to its real document,
//! under the plugin's own area of the window. Events go the other way as [`Event`]s,
//! and the few reads only the host can answer (layout, live values, focus) as
//! [`Query`]s. Every message is postcard bytes ([`encode`], [`decode`]).
//!
//! Buffers: the guest exports [`exports::ALLOC`] and [`exports::FREE`]. The host puts
//! a message into guest memory with `alloc` and passes `(ptr, len)`; a guest export
//! that answers bytes returns them packed as `ptr << 32 | len` ([`pack`]), and the
//! host frees them once read. A guest import that answers bytes ([`imports::QUERY`])
//! returns their length and keeps them until the guest copies them out with
//! [`imports::TAKE_ANSWER`], so the host never calls into the guest while the guest
//! is calling it.
//!
//! The Pimble API crosses in [`api`]: a plugin's `Backend` call is an
//! [`imports::API_CALL`] naming a call id, and its answer comes back later through
//! [`exports::RESOLVE`] (docs/PLUGINS_CONTRACT.md wave 2). Those messages are JSON
//! (see the module for why).
//!
//! Every export and import name carries a `pimble_` prefix: a wasip1 module links
//! wasi-libc, and a plain `free` export or `log` import would collide with libc's
//! `free` and libm's `log` at link time.

use serde::{Deserialize, Serialize};

pub mod api;

/// The version of this ABI. A host runs only plugins built for the same version;
/// anything else is refused at load with [`refusal_for_abi`]'s sentence. Bump it on
/// any change a plugin built before it would misread: a message's shape, an export,
/// an import, the meaning of a node id.
pub const ABI_VERSION: u32 = 2;

/// The guest node id of the plugin document's `<html>` root. Nothing in the host
/// stands for it: a plugin renders under its body.
pub const GUEST_ROOT: u64 = 0;
/// The guest node id of the plugin document's `<body>`: the plugin's area of the
/// window. A plugin's view is appended to it, and so is anything a component
/// portals to `body()` (a menu, a modal), so those stay inside the plugin's area.
pub const GUEST_BODY: u64 = 1;
/// The first id a guest gives a node it creates.
pub const FIRST_GUEST_NODE: u64 = 2;

/// The guest's exports, by name.
pub mod exports {
    /// `()`: the reactor's entry (static constructors). The host calls it once,
    /// right after instantiating, before any other export.
    pub const INITIALIZE: &str = "_initialize";
    /// `(len: u32) -> ptr: u32`: memory for a message the host is about to write.
    /// The host frees it with `FREE` once the call it was for returns.
    pub const ALLOC: &str = "pimble_alloc";
    /// `(ptr: u32, len: u32)`: give back what `ALLOC` or a packed answer handed out.
    pub const FREE: &str = "pimble_free";
    /// `() -> u64`: the plugin's [`Manifest`](crate::Manifest), packed.
    pub const DESCRIBE: &str = "pimble_describe";
    /// `(ptr: u32, len: u32)`: a [`MountArgs`](crate::MountArgs). Renders the viewer it names
    /// into the guest body.
    pub const MOUNT: &str = "pimble_mount";
    /// `(ptr: u32, len: u32) -> u32`: an [`Event`](crate::Event); 1 when the plugin
    /// consumed it (a key or a wheel), else 0.
    pub const DISPATCH: &str = "pimble_dispatch";
    /// `(ptr: u32, len: u32)`: new [`Props`](crate::Props) for the mounted view.
    pub const PROPS_CHANGED: &str = "pimble_props_changed";
    /// `()`: take the view down and drop what it holds.
    pub const UNMOUNT: &str = "pimble_unmount";
    /// `(id: u64)`: the timer the guest armed with `SET_TIMEOUT` is due.
    pub const FIRE_TIMEOUT: &str = "pimble_fire_timeout";
    /// `(id: u64)`: poll the task the guest scheduled with `SCHEDULE_TASK`.
    pub const POLL_TASK: &str = "pimble_poll_task";
    /// `() -> i64`: sample the guest's polled signals; answers the milliseconds
    /// until the next is due, 0 for every frame, -1 for none.
    pub const DRAIN_POLLS: &str = "pimble_drain_polls";
    /// `(ptr: u32, len: u32)`: an [`api::Resolution`](crate::api::Resolution), the
    /// answer to one of the guest's API calls (JSON). The host never calls it
    /// inside the guest's own call: an answer is always a later turn.
    pub const RESOLVE: &str = "pimble_resolve";
    /// The guest's linear memory.
    pub const MEMORY: &str = "memory";
}

/// The host's imports, all in module [`imports::MODULE`].
pub mod imports {
    /// The import module every host function lives in.
    pub const MODULE: &str = "pimble";
    /// `(ptr: u32, len: u32)`: apply a list of [`Op`](crate::Op)s now. A list the
    /// host refuses (a node that is not the plugin's) stops the plugin.
    pub const FLUSH: &str = "pimble_flush";
    /// `(ptr: u32, len: u32) -> u32`: a [`Query`](crate::Query); answers the length of
    /// the [`Answer`](crate::Answer), which the guest copies out with `TAKE_ANSWER`.
    pub const QUERY: &str = "pimble_query";
    /// `(ptr: u32)`: copy the last answer into guest memory at `ptr`.
    pub const TAKE_ANSWER: &str = "pimble_take_answer";
    /// `(id: u64, delay_ms: u32)`: call `FIRE_TIMEOUT(id)` after `delay_ms`.
    pub const SET_TIMEOUT: &str = "pimble_set_timeout";
    /// `(id: u64)`: call `POLL_TASK(id)` soon, after the current turn.
    pub const SCHEDULE_TASK: &str = "pimble_schedule_task";
    /// `()`: a polled signal was registered; call `DRAIN_POLLS` soon.
    pub const WAKE_POLLS: &str = "pimble_wake_polls";
    /// `(call: u64, ptr: u32, len: u32)`: an [`api::ApiCall`](crate::api::ApiCall)
    /// (JSON) under the guest's own call id. Answers nothing now: the host checks
    /// it against the plugin's approval, forwards it, and later calls `RESOLVE`
    /// with the call id (a refusal too).
    pub const API_CALL: &str = "pimble_api_call";
    /// `(call: u64)`: the guest no longer wants answers to that call (a
    /// subscription it dropped).
    pub const API_CANCEL: &str = "pimble_api_cancel";
    /// `(level: u32, ptr: u32, len: u32)`: a line for the app's log, UTF-8. Level 0
    /// is an error, 1 a warning, 2 information, 3 and above debugging.
    pub const LOG: &str = "pimble_log";
}

/// `ptr << 32 | len`: how an export answers a buffer.
pub fn pack(ptr: u32, len: u32) -> u64 {
    ((ptr as u64) << 32) | len as u64
}

/// The `(ptr, len)` [`pack`] packed.
pub fn unpack(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

/// A message as bytes.
pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    postcard::to_allocvec(value).expect("postcard encodes every ABI message")
}

/// A message from bytes, or why they are not one.
pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, String> {
    postcard::from_bytes(bytes).map_err(|e| e.to_string())
}

// ── The manifest ──────────────────────────────────────────────────────────────

/// What a plugin is and provides, answered by its `describe` export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// A stable id, unique among plugins: `hello-viewer`.
    pub id: String,
    /// What a person reads: "Hello viewer".
    pub name: String,
    /// The plugin's own version.
    pub version: String,
    /// The [`ABI_VERSION`] the plugin was built for (the SDK fills it in).
    pub abi: u32,
    /// The rinch git revision the plugin was built against (the SDK fills it in from
    /// the plugin's `Cargo.lock`), or `"unknown"`.
    pub rinch: String,
    /// What it provides.
    pub provides: Vec<Provision>,
    /// What it asks to be allowed (decision 2): the person approves it once per
    /// device, and the host refuses any call outside what was approved.
    pub asks: Permissions,
}

/// What a plugin asks to be allowed to do through the Pimble API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Permissions {
    /// Which stores it reads (and writes, with `write`).
    pub stores: StoreAsk,
    /// It changes what it reads, not only shows it.
    pub write: bool,
    /// It reaches the network. No host grants this yet (decision 6): a plugin
    /// that asks for it is refused at load.
    pub network: bool,
}

/// Which stores a plugin asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StoreAsk {
    /// None: it reads nothing through the API.
    #[default]
    None,
    /// Every store open on the device, now and later.
    All,
    /// The stores the person chooses when approving it (the store of the
    /// metanode it is first shown for, and each further one as it is shown
    /// there).
    Chosen,
}

impl Permissions {
    /// Whether it asks for anything at all: a plugin asking for nothing is
    /// shown without an approval.
    pub fn asks_anything(&self) -> bool {
        self.stores != StoreAsk::None || self.write || self.network
    }
}

/// One thing a plugin provides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Provision {
    /// A metanode viewer (docs/METANODES_CONTRACT.md).
    Viewer(ViewerDecl),
}

/// A viewer a plugin provides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewerDecl {
    /// The id a metanode names in `metanode:viewer`: `hello`, `kanban`.
    pub id: String,
    /// What a person reads in a menu: "Hello".
    pub name: String,
    /// The source shapes it can show (docs/METANODES_CONTRACT.md section 1.2).
    pub takes: Vec<SourceShape>,
}

/// A shape of source a viewer can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceShape {
    /// Whatever the source finds.
    Any,
    /// The metanode's own children.
    Children,
    /// The metanode's subtree, at least `depth` levels deep.
    Subtree { depth: u32 },
}

impl Manifest {
    /// The viewers this plugin provides.
    pub fn viewers(&self) -> impl Iterator<Item = &ViewerDecl> {
        self.provides.iter().map(|p| match p {
            Provision::Viewer(v) => v,
        })
    }
}

/// The sentence a person reads when a plugin built for another ABI is refused.
pub fn refusal_for_abi(name: &str, abi: u32) -> String {
    format!("The {name} plugin was built for plugin ABI {abi}, and this Pimble runs ABI {ABI_VERSION} plugins.")
}

// ── Mounting ──────────────────────────────────────────────────────────────────

/// What `mount` is handed: which of the plugin's viewers, and what it shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MountArgs {
    pub viewer: String,
    pub props: Props,
}

/// What a viewer is handed: the metanode it shows and the viewer's settings on
/// it. The viewer reads the metanode's source (`Pimble::metanode_source`) and
/// everything else through the Pimble API, which keeps it live; the props change
/// only when the settings do.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Props {
    /// The metanode.
    pub metanode: NodeAddr,
    /// The viewer's shared settings on the node (its `data` under the viewer's
    /// id), as JSON, when it has any.
    pub settings: Option<String>,
}

/// A node by its store and its id, as text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct NodeAddr {
    pub store: String,
    pub node: String,
}

impl NodeAddr {
    /// The API's address, when both ids parse.
    pub fn addr(&self) -> Option<pimble_api::Addr> {
        let store = pimble_core::StoreId::parse(&self.store).ok()?;
        let node = pimble_core::NodeId::parse(&self.node).ok()?;
        Some(pimble_api::Addr::new(store, node))
    }
}

impl From<pimble_api::Addr> for NodeAddr {
    fn from(a: pimble_api::Addr) -> Self {
        NodeAddr { store: a.store.to_string(), node: a.node.to_string() }
    }
}

// ── The DOM op log ────────────────────────────────────────────────────────────

/// One DOM write a guest made, in the order it made them. Node ids are the guest's
/// own ([`GUEST_BODY`] and the ids its `Create*` ops introduced); the host maps them
/// to its nodes and refuses any other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Op {
    CreateElement { id: u64, tag: String },
    CreateText { id: u64, text: String },
    CreateComment { id: u64, text: String },
    Append { parent: u64, child: u64 },
    InsertBefore { parent: u64, child: u64, reference: u64 },
    InsertAt { parent: u64, child: u64, index: u32 },
    RemoveChild { parent: u64, child: u64 },
    Replace { old: u64, new: u64 },
    /// Detached, still re-insertable.
    Remove { node: u64 },
    /// Detached for good with its subtree: the host lets go of the ids.
    Discard { node: u64 },
    SetText { node: u64, text: String },
    SetAttribute { node: u64, name: String, value: String },
    RemoveAttribute { node: u64, name: String },
    SetStyle { node: u64, property: String, value: String },
    SetScrollTop { node: u64, value: f64 },
    SetScrollLeft { node: u64, value: f64 },
    Focus { node: u64 },
    /// `direction`: 0 forward, 1 backward, 2 none.
    SetSelection { node: u64, start: u32, end: u32, direction: u8 },
    ScrollIntoView { node: u64 },
}

// ── Events ────────────────────────────────────────────────────────────────────

/// What kind of handler an event attribute holds, and so what its event carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandlerKind {
    /// `Fn()`: a click, a press, a hover, a drag step. Carries the click context.
    Plain,
    /// `Fn(String)`: `oninput`, `onchange`.
    Input,
    /// `Fn(ScrollEvent)`.
    Scroll,
    /// `Fn(&KeyEventData) -> bool`: answered synchronously.
    Key,
    /// `Fn(&WheelEvent) -> bool`: answered synchronously.
    Wheel,
    /// Files dropped from the operating system. A plugin is given no files (no
    /// install permission covers them yet), so the host never wires these.
    Files,
}

/// rinch's closed list of event attributes (`rinch-macros` `HTML_EVENTS`: the
/// attribute an `rsx!` `on*` prop writes its handler id to), with the kind of
/// handler each holds. The host rewrites exactly these: a guest's handler id in one
/// of them becomes a host handler of the host's own that calls the guest's
/// `dispatch`. Any other attribute passes through as data.
pub const HANDLER_ATTRIBUTES: &[(&str, HandlerKind)] = &[
    ("data-rid", HandlerKind::Plain),
    ("data-oninput", HandlerKind::Input),
    ("data-onchange", HandlerKind::Input),
    ("data-onscroll", HandlerKind::Scroll),
    ("data-onfiledrop", HandlerKind::Files),
    ("data-ondropfiles", HandlerKind::Files),
    ("data-onkeydown", HandlerKind::Key),
    ("data-onkeyup", HandlerKind::Key),
    ("data-onwheel", HandlerKind::Wheel),
    ("data-onsubmit", HandlerKind::Plain),
    ("data-onmousedown", HandlerKind::Plain),
    ("data-onmouseup", HandlerKind::Plain),
    ("data-onmousemove", HandlerKind::Plain),
    ("data-onenter", HandlerKind::Plain),
    ("data-onleave", HandlerKind::Plain),
    ("data-oncontextmenu", HandlerKind::Plain),
    ("data-ondragstart", HandlerKind::Plain),
    ("data-ondragmove", HandlerKind::Plain),
    ("data-ondragenter", HandlerKind::Plain),
    ("data-ondragover", HandlerKind::Plain),
    ("data-ondragleave", HandlerKind::Plain),
    ("data-ondrop", HandlerKind::Plain),
    ("data-ondragend", HandlerKind::Plain),
    ("data-onfiledragenter", HandlerKind::Plain),
    ("data-onfiledragleave", HandlerKind::Plain),
];

/// The kind of handler attribute `name` holds, or `None` for an ordinary attribute.
pub fn handler_kind(name: &str) -> Option<HandlerKind> {
    HANDLER_ATTRIBUTES.iter().find(|(attr, _)| *attr == name).map(|(_, kind)| *kind)
}

/// An event for one of the guest's handlers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// The guest's own handler id, as it wrote it into the attribute.
    pub handler: u64,
    pub data: EventData,
}

/// What an event carries, by the kind of handler.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventData {
    Plain(Click),
    /// The field's text; `node` is the guest id of the field, when the host knew it.
    Input { value: String, node: Option<u64> },
    Scroll { top: f64, left: f64 },
    Key(Key),
    Wheel(Wheel),
}

/// Shift, Ctrl, Alt and Meta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

/// rinch's click context, in the host window's logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Click {
    pub mouse_x: f32,
    pub mouse_y: f32,
    pub element_x: f32,
    pub element_y: f32,
    pub element_width: f32,
    pub element_height: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
    /// 0 left, 1 middle, 2 right.
    pub button: u8,
    pub modifiers: Modifiers,
}

/// A key press or release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Key {
    /// `KeyboardEvent.key`.
    pub key: String,
    /// `KeyboardEvent.code`.
    pub code: String,
    pub modifiers: Modifiers,
    /// A release, else a press.
    pub up: bool,
}

/// A wheel turn over an element.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Wheel {
    pub delta_x: f32,
    pub delta_y: f32,
    pub x: f32,
    pub y: f32,
    pub modifiers: Modifiers,
}

// ── Reads only the host can answer ────────────────────────────────────────────

/// A read the guest's shadow tree cannot answer. The guest flushes its op log
/// before asking, so the host's document holds everything the guest wrote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Query {
    /// The text a form control holds now (`DomDocument::live_value`).
    LiveValue { node: u64 },
    /// The border box, `(x, y, width, height)` (`query_node_layout`).
    Layout { node: u64 },
    /// The scroll offsets and sizes.
    Scroll { node: u64 },
    /// The focused node, when it is one of the guest's.
    ActiveElement,
    /// The window's size.
    ViewportSize,
    /// A text control's selection.
    TextSelection { node: u64 },
}

/// The host's answer to a [`Query`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Answer {
    /// Nothing to say: the node is not the guest's, or the backend cannot tell.
    Nothing,
    Text(String),
    Rect { x: f32, y: f32, width: f32, height: f32 },
    Scroll { top: f64, left: f64, height: f64, width: f64, client_height: f64, client_width: f64 },
    Node(u64),
    Size { width: f32, height: f32 },
    /// `direction`: 0 forward, 1 backward, 2 none.
    Selection { start: u32, end: u32, direction: u8 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_through_postcard() {
        let manifest = Manifest {
            id: "hello-viewer".into(),
            name: "Hello viewer".into(),
            version: "0.1.0".into(),
            abi: ABI_VERSION,
            rinch: "edcee04".into(),
            provides: vec![Provision::Viewer(ViewerDecl {
                id: "hello".into(),
                name: "Hello".into(),
                takes: vec![SourceShape::Any, SourceShape::Subtree { depth: 2 }],
            })],
            asks: Permissions { stores: StoreAsk::Chosen, write: true, network: false },
        };
        assert_eq!(decode::<Manifest>(&encode(&manifest)).unwrap(), manifest);
        let ops = vec![
            Op::CreateElement { id: 2, tag: "div".into() },
            Op::SetAttribute { node: 2, name: "data-rid".into(), value: "7".into() },
            Op::Append { parent: GUEST_BODY, child: 2 },
        ];
        assert_eq!(decode::<Vec<Op>>(&encode(&ops)).unwrap(), ops);
        let event = Event { handler: 7, data: EventData::Key(Key { key: "a".into(), code: "KeyA".into(), modifiers: Modifiers::default(), up: false }) };
        assert_eq!(decode::<Event>(&encode(&event)).unwrap(), event);
        assert!(decode::<Manifest>(&[1, 2, 3]).is_err());
    }

    #[test]
    fn packing_keeps_both_halves() {
        assert_eq!(unpack(pack(0xdead_beef, 42)), (0xdead_beef, 42));
    }

    #[test]
    fn the_handler_attributes_are_rinchs_closed_list() {
        assert_eq!(HANDLER_ATTRIBUTES.len(), 25);
        assert_eq!(handler_kind("data-rid"), Some(HandlerKind::Plain));
        assert_eq!(handler_kind("data-onkeydown"), Some(HandlerKind::Key));
        assert_eq!(handler_kind("class"), None);
    }
}
