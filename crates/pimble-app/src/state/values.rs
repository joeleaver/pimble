//! The small values `AppStore`'s signals carry that belong to no one part of
//! it: the connection, the search box, a followed link and its tooltip, the
//! plugins' recipes, the account answer, updates, the explorer's width.

use super::*;

/// Current state of the search box: idle (query empty), the last successful
/// results (possibly empty), or an error/status message from the server (a
/// connection failure, or the index still building — `SearchError::IndexBuilding`
/// arrives as a plain RPC error whose text already says so, so it displays as-is).
#[derive(Debug, Clone, Default)]
pub enum SearchState {
    #[default]
    Idle,
    Results(Vec<pimble_rpc::SearchResultItem>),
    Error(String),
}

/// What the link tooltip shows and where: under the hovered link, `x`/`y`
/// in the coordinates `position: fixed` uses.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkTooltip {
    /// The target: a note's title and store, a sentence, or a web link's URL.
    pub target: String,
    /// The line under it: how to open the link (nothing for a mention).
    pub hint: &'static str,
    pub x: f32,
    pub y: f32,
}

/// A recipe an installed plugin registers, and the plugin's name.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginRecipe {
    pub plugin: String,
    pub recipe: pimble_plugin_abi::RecipeDecl,
}

/// Where a followed link leads, as `AppStore::link_open` carries it.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkOpen {
    /// `node_{store}_{node}`, as a search hit opens.
    pub value: String,
    pub store_id: StoreId,
    pub node_id: NodeId,
    pub anchor: Option<pimble_core::Anchor>,
    /// The pane the link was followed from, which is where it opens
    /// (docs/SPLIT_VIEW_CONTRACT.md); the focused pane when it was followed
    /// from none.
    pub pane: Option<PaneId>,
}

/// The explorer's width before anyone drags it, and the range a drag keeps it in:
/// wide enough to read a title, narrow enough to leave the document room.
pub const SIDEBAR_DEFAULT_WIDTH: f32 = 260.0;

pub const SIDEBAR_MIN_WIDTH: f32 = 160.0;

pub const SIDEBAR_MAX_WIDTH: f32 = 640.0;

/// Whether the server said what account it holds (`AppStore::cloud_answer`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CloudAnswer {
    /// Asked, or about to be: "connecting...".
    #[default]
    Waiting,
    /// Answered (`CloudStatusChanged`).
    Answered,
    /// Asking failed, with why: "disconnected".
    Failed(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connected,
    Error(String),
}

/// Where keeping this copy current stands (docs/UPDATE_CONTRACT.md), as
/// Help > About shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateStatus {
    /// Not the installed app (a development build, a copy run from where it
    /// was unpacked, the browser): nothing updates it.
    NotInstalled,
    /// The first check runs half a minute after start.
    Waiting,
    Checking,
    /// The last check found nothing newer; when it ran, as shown.
    UpToDate { at: String },
    /// A newer version is downloaded, checked and waiting for a restart.
    Ready { version: String },
    /// The last check failed; the reason and when.
    Failed { reason: String, at: String },
}

/// What the server this window talks to says it is.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerVersion {
    Known(String),
    /// A server from before 0.12.2, which does not say.
    Unknown,
}
