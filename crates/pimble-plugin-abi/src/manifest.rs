//! The manifest: what a plugin is and provides, answered by its `describe` export.

use serde::{Deserialize, Serialize};
use crate::ABI_VERSION;

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
    /// A viewer for the children of another of the plugin's viewers'
    /// metanodes: a kanban's column opened on its own shows as a column.
    ChildViewer(ChildViewerDecl),
}

/// A viewer a plugin provides for the children of its own viewer's
/// metanodes. Nothing is stored on such a child: the app shows a node with
/// it because of where the node is (its parent is a metanode shown with
/// [`ChildViewerDecl::of`]), and the node gets it back or loses it as it
/// moves. It is mounted as any viewer, with the child as the props'
/// metanode, whose source is its children.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildViewerDecl {
    /// Unique among the plugin's viewers: `kanban-column`.
    pub id: String,
    /// What a person reads in a menu: "Kanban column".
    pub name: String,
    /// The plugin's own viewer whose metanodes' children it shows: `kanban`.
    pub of: String,
    /// The icon the children's rows show when they have none of their own
    /// (a Tabler icon name): "layout-columns".
    pub icon: Option<String>,
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

    /// The viewers this plugin provides for its viewers' metanodes' children.
    pub fn child_viewers(&self) -> impl Iterator<Item = &ChildViewerDecl> {
        self.provides.iter().filter_map(|p| match p {
            Provision::ChildViewer(c) => Some(c),
            _ => None,
        })
    }

    /// Whether the plugin mounts viewer `id`: one of its viewers, or of its
    /// child viewers.
    pub fn mounts(&self, id: &str) -> bool {
        self.viewers().any(|v| v.id == id) || self.child_viewers().any(|c| c.id == id)
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
