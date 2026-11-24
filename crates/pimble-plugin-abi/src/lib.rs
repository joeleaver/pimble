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
mod events;
pub mod guide;
mod host;
mod manifest;
mod mount;
mod ops;
mod query;

pub use events::{dispatched, handler_kind, Click, Event, EventData, HandlerKind, Key, Modifiers, Wheel, HANDLER_ATTRIBUTES};
pub use host::{HostRequest, HostState, NodePeople, PresentPerson};
pub use manifest::{refusal_for_abi, ChildViewerDecl, Manifest, Permissions, Provision, RecipeDecl, SourceShape, StoreAsk, ViewerDecl};
pub use mount::{MountArgs, NodeAddr, Props, MAX_REMEMBERED};
pub use ops::Op;
pub use query::{Answer, Query};

/// The version of this ABI. A host runs only plugins built for the same version;
/// anything else is refused at load with [`refusal_for_abi`]'s sentence. Bump it on
/// any change a plugin built before it would misread: a message's shape, an export,
/// an import, the meaning of a node id.
pub const ABI_VERSION: u32 = 7;

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
                Provision::ChildViewer(ChildViewerDecl {
                    id: "kanban-column".into(),
                    name: "Kanban column".into(),
                    of: "kanban".into(),
                    icon: Some("layout-columns".into()),
                }),
            ],
            asks: Permissions { stores: StoreAsk::Chosen, write: true, network: false },
        };
        assert_eq!(decode::<Manifest>(&encode(&manifest)).unwrap(), manifest);
        assert_eq!(manifest.viewers().count(), 1);
        assert_eq!(manifest.recipes().next().map(|r| r.children.len()), Some(3));
        assert_eq!(manifest.child_viewers().map(|c| c.of.as_str()).collect::<Vec<_>>(), ["kanban"]);
        assert!(manifest.mounts("hello") && manifest.mounts("kanban-column") && !manifest.mounts("kanban"));
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
}
