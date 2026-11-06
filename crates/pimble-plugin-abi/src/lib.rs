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
pub mod guide;

/// The version of this ABI. A host runs only plugins built for the same version;
/// anything else is refused at load with [`refusal_for_abi`]'s sentence. Bump it on
/// any change a plugin built before it would misread: a message's shape, an export,
/// an import, the meaning of a node id.
pub const ABI_VERSION: u32 = 6;

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
    /// `(ptr: u32, len: u32) -> u32`: an [`Event`](crate::Event); answers the
    /// [`dispatched`](crate::dispatched) flags: whether the plugin consumed it (a
    /// key or a wheel), and what of the window's own machinery the handler asked
    /// for (ABI 4).
    pub const DISPATCH: &str = "pimble_dispatch";
    /// `(ptr: u32, len: u32)`: new [`Props`](crate::Props) for the mounted view.
    pub const PROPS_CHANGED: &str = "pimble_props_changed";
    /// `()`: take the view down and drop what it holds.
    pub const UNMOUNT: &str = "pimble_unmount";
    /// `()`: an element drag in the host's window ended, dropped or cancelled
    /// (ABI 4). The guest tells rinch's `on_element_drag_end` listeners: a
    /// drag whose dragged element its view re-rendered away hears no
    /// `data-ondragend` (the host sends it to the gone node), and a sortable
    /// list keeps state for the drag until it hears this.
    pub const DRAG_ENDED: &str = "pimble_drag_ended";
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
    /// `(ptr: u32, len: u32)`: a [`HostState`](crate::HostState), what the
    /// host knows that the plugin draws with and that changes on its own: who
    /// is in which node, whom the pane follows, the theme. Sent after
    /// `MOUNT` and again whenever any of it changes (ABI 3).
    pub const HOST_STATE: &str = "pimble_host_state";
    /// `(ptr: u32, len: u32) -> u64`: start a tool (docs/AGENT_GUIDES_CONTRACT.md
    /// section 8), a JSON [`guide::ToolRequest`](crate::guide::ToolRequest), as a
    /// task in the guest; answers the tool run's id. Optional: a plugin without
    /// it has no tools, and its absence changes nothing else.
    pub const TOOL: &str = "pimble_tool";
    /// `(id: u64) -> u64`: the finished tool run's JSON
    /// [`guide::ToolAnswer`](crate::guide::ToolAnswer), packed, or 0 while it
    /// runs. Optional, with `TOOL`.
    pub const TOOL_RESULT: &str = "pimble_tool_result";
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
    /// `(ptr: u32, len: u32)`: a [`HostRequest`](crate::HostRequest), something
    /// only the app can do for the person (open a node in the pane, say where
    /// they are, follow someone). Answers nothing; the host may decline (ABI 3).
    pub const HOST: &str = "pimble_host";
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
    /// A way to make a new node, offered in the explorer's new-node menu
    /// ("New Kanban").
    Recipe(RecipeDecl),
}

/// A recipe a plugin registers: what a new node of its kind starts as, as
/// data the app makes through its own API when the person picks it. No
/// plugin code runs to make it, so a recipe needs nothing allowed; the node
/// it makes is the person's own edit, made where they chose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeDecl {
    /// Unique among the plugin's recipes: `new-kanban`.
    pub id: String,
    /// The menu item: "New Kanban".
    pub label: String,
    /// The new node's title: "Kanban".
    pub title: String,
    /// The new node's icon (a Tabler icon name), and the menu item's.
    pub icon: Option<String>,
    /// The new node is a metanode shown with this viewer (one of the
    /// plugin's own, usually).
    pub viewer: String,
    /// Its source: its own subtree this many levels deep (1 is its
    /// children).
    pub depth: u32,
    /// The children it starts with, titled, in order: a kanban's columns.
    pub children: Vec<String>,
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
        self.provides.iter().filter_map(|p| match p {
            Provision::Viewer(v) => Some(v),
            _ => None,
        })
    }

    /// The recipes this plugin registers.
    pub fn recipes(&self) -> impl Iterator<Item = &RecipeDecl> {
        self.provides.iter().filter_map(|p| match p {
            Provision::Recipe(r) => Some(r),
            _ => None,
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
    /// What this device remembers of how the viewer last looked on this
    /// metanode, as the viewer handed it to [`HostRequest::Remember`]: one
    /// person's view on one device (lanes collapsed, columns folded), never
    /// in a store and never synced, like the explorer's expanded rows.
    pub remembered: Option<String>,
}

/// The most a viewer may ask the host to remember for one metanode, in bytes.
pub const MAX_REMEMBERED: usize = 16 * 1024;

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

// ── What the host knows, and what it does for a plugin (ABI 3) ────────────────

/// What the host knows that a plugin draws with and that changes on its own,
/// sent whole whenever any of it changes (`pimble_host_state`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HostState {
    /// The app shows its dark theme: colors are drawn for it
    /// (`pimble_ui::appearance::display_color`).
    pub dark: bool,
    /// Who else is in which node, for every node someone is in, in the stores
    /// the plugin may read (docs/PRESENCE_CONTRACT.md "Plugins"): resolved
    /// as the app resolves them, names, initials and colors included, so a
    /// plugin draws them with `pimble_ui::presence` exactly as the app does.
    pub people: Vec<NodePeople>,
    /// The key of the person the plugin's pane follows, when it follows
    /// someone: the plugin shows where they are in its view (a kanban
    /// scrolls to the card they are on).
    pub following: Option<String>,
}

/// The people in one node.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodePeople {
    pub node: NodeAddr,
    pub people: Vec<PresentPerson>,
}

/// A person in a node, as the app draws them (`pimble_ui::presence::Person`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PresentPerson {
    pub key: String,
    pub name: String,
    pub initials: String,
    pub color: String,
    pub title: String,
    pub agent: bool,
}

impl HostState {
    /// The people in `node`.
    pub fn people_in(&self, node: &NodeAddr) -> &[PresentPerson] {
        self.people.iter().find(|p| &p.node == node).map_or(&[], |p| p.people.as_slice())
    }
}

/// Something only the app can do for the person using a plugin
/// (`pimble_host`). The host may decline any of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HostRequest {
    /// Open `node` in the plugin's pane, as a click on a row of the outline
    /// does.
    Open { node: NodeAddr },
    /// The person is on `node` in the plugin's view (a card they chose), or
    /// on nothing in it: the app says so to everyone else as part of the
    /// pane's presence, so they show on that node as they would in a native
    /// view. Never document data.
    Focus { node: Option<NodeAddr> },
    /// Follow the person with this key in the plugin's pane, as a click on
    /// their initials on the pane's strip does.
    Follow { person: String },
    /// Remember `value` (the viewer's own text, at most [`MAX_REMEMBERED`]
    /// bytes) as how the viewer looks on its metanode on this device; the
    /// next mount's [`Props::remembered`]. Not document data.
    Remember { value: String },
    /// Open `node` beside the plugin's pane (ABI 6): in the pane the app keeps
    /// beside it, split off to the right the first time, while the plugin's pane
    /// keeps the focus. Where there is no room for another pane it opens in
    /// another pane shown, or in the plugin's own when that is the only one.
    OpenBeside { node: NodeAddr },
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

// rinch's sortable lists (joeleaver/rinch#2093) run on handlers already in this
// list (`data-ondragstart`, `-ondragover`, `-ondragleave`, `-ondrop`,
// `-ondragmove`, `-ondragend`, `data-onkeydown`) and judge a drop inside the
// guest, synchronously, with what the guest holds: no new kind was needed. What
// they need of the host besides is the click context of each drag step, the
// painted box of an element ([`Query::BoundingRect`]), the element a key was sent
// to ([`Key::target`]), the end of every drag ([`exports::DRAG_ENDED`]), and the
// drag ghost hidden for a source marked `data-drag-ghost="false"` (plain data the
// host reads itself).

/// The kind of handler attribute `name` holds, or `None` for an ordinary attribute.
pub fn handler_kind(name: &str) -> Option<HandlerKind> {
    HANDLER_ATTRIBUTES.iter().find(|(attr, _)| *attr == name).map(|(_, kind)| *kind)
}

/// The flags `dispatch` answers (ABI 4).
pub mod dispatched {
    /// A key or wheel handler consumed the event. The other bits are free for
    /// what a later handler may ask of the host's window.
    pub const CONSUMED: u32 = 1;
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
    /// The guest node the key was sent to (the focused element), when it is
    /// one of the guest's (ABI 4): an element handler hears keys typed in the
    /// fields inside it too, and this tells them apart (rinch
    /// `KeyEventData::target`).
    pub target: Option<u64>,
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
    /// The layout box relative to its parent, `(x, y, width, height)`
    /// (`query_node_layout`).
    Layout { node: u64 },
    /// The scroll offsets and sizes.
    Scroll { node: u64 },
    /// The focused node, when it is one of the guest's.
    ActiveElement,
    /// The window's size.
    ViewportSize,
    /// A text control's selection.
    TextSelection { node: u64 },
    /// The box the element is painted in, in the window's logical pixels: the
    /// frame of the click context's pointer and element box
    /// (`DomDocument::bounding_rect`, ABI 4). A drag compares the pointer
    /// with the boxes of items it is not over; the parent-relative `Layout`
    /// cannot be compared with it.
    BoundingRect { node: u64 },
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
            provides: vec![
                Provision::Viewer(ViewerDecl {
                    id: "hello".into(),
                    name: "Hello".into(),
                    takes: vec![SourceShape::Any, SourceShape::Subtree { depth: 2 }],
                }),
                Provision::Recipe(RecipeDecl {
                    id: "new-kanban".into(),
                    label: "New Kanban".into(),
                    title: "Kanban".into(),
                    icon: Some("layout-kanban".into()),
                    viewer: "kanban".into(),
                    depth: 2,
                    children: vec!["To do".into(), "Doing".into(), "Done".into()],
                }),
            ],
            asks: Permissions { stores: StoreAsk::Chosen, write: true, network: false },
        };
        assert_eq!(decode::<Manifest>(&encode(&manifest)).unwrap(), manifest);
        assert_eq!(manifest.viewers().count(), 1);
        assert_eq!(manifest.recipes().next().map(|r| r.children.len()), Some(3));
        let state = HostState {
            dark: true,
            people: vec![NodePeople { node: NodeAddr { store: "s".into(), node: "n".into() }, people: vec![PresentPerson { key: "k".into(), agent: true, ..Default::default() }] }],
            following: Some("k".into()),
        };
        assert_eq!(decode::<HostState>(&encode(&state)).unwrap(), state);
        assert_eq!(state.people_in(&NodeAddr { store: "s".into(), node: "n".into() }).len(), 1);
        let request = HostRequest::Focus { node: Some(NodeAddr { store: "s".into(), node: "n".into() }) };
        assert_eq!(decode::<HostRequest>(&encode(&request)).unwrap(), request);
        let ops = vec![
            Op::CreateElement { id: 2, tag: "div".into() },
            Op::SetAttribute { node: 2, name: "data-rid".into(), value: "7".into() },
            Op::Append { parent: GUEST_BODY, child: 2 },
        ];
        assert_eq!(decode::<Vec<Op>>(&encode(&ops)).unwrap(), ops);
        let event = Event { handler: 7, data: EventData::Key(Key { key: "a".into(), code: "KeyA".into(), modifiers: Modifiers::default(), up: false, target: Some(4) }) };
        assert_eq!(decode::<Event>(&encode(&event)).unwrap(), event);
        let query = Query::BoundingRect { node: 7 };
        assert_eq!(decode::<Query>(&encode(&query)).unwrap(), query);
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
