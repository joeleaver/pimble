//! The canvas tools (docs/MCP_CONTRACT.md "Canvases"; the model is
//! docs/IMAGE_MARKUP_CONTRACT.md section 3, the words docs/CANVAS_CONTRACT.md).
//! A canvas is a board in the code: a canvas node's `data.board`, or a
//! picture's canvas in a note's text, `data["board:<id>"]`. Every write is one
//! `pimble_board` transaction on the node's document, sent through `applyEdit`
//! (`Pimble::edit_doc`), so it merges with whoever is drawing. An agent's
//! elements are stamped with its own id and name, as a person's are with
//! theirs (contract section 6), and the agent shows on the canvas with what it
//! just made selected (docs/PRESENCE_CONTRACT.md section 8).
//!
//! What the app's tools refuse a person these refuse an agent: locks are
//! `pimble_board`'s to judge (`BoardTxn` refuses what a lock guards), and
//! [`said`] tells an agent which tool takes the lock off.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use base64::Engine;
use pimble_board::recipe::{StartingPicture, CARD_SIZE, PICTURE_LAYER_NAME, STICKY_SIZE};
use pimble_board::{
    dash_pattern_text, parse_dash_pattern, selection_bounds, Align, ArrowHeads, Author, Blend, Board, BoardError, BoardSnapshot,
    BoardTxn, Cap, CardLabel, Color, CombineOp, Dash, End, FlipAxis, Geom, Gradient, GradientKind, GradientStop, Join, Kind, LayerKind,
    NewElement, NewStroke, Page, PaintBody, PathGeom, PathRule, Picture, PictureGeom, Placement, Point, Rect, Side, Stamp, StrokeBlend, Style,
    StyleKey, SvgPlacement, Transform, ViewOptions,
};
use pimble_board::path::{parse_d, to_d};
use pimble_core::{node_types, BlobUrl, Node, Place, PimbleUrl};
use pimble_crdt::NodeDoc;
use pimble_rpc::{BoardPoint, BoardPresence, BoardRect};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use pimble_api::{label, recipes, Addr};

use super::{type_name, Pimble};

/// Board writes are stamped with RFC 3339 UTC with milliseconds: step numbers
/// sort by it as a string, so every writer uses the app's shape.
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// How many elements get_canvas lists before it stops.
const LIST_LIMIT: usize = 400;
/// The longest path data get_canvas writes out for one path.
const PATH_TEXT_LIMIT: usize = 4000;

/// What a list of specs needs from outside the document, by spec index:
/// each picture's URL and pixel size, each card's link and title.
type Prepared = (HashMap<usize, (String, u32, u32)>, HashMap<usize, (String, String)>);

// ── Parameters ───────────────────────────────────────────────────────────

#[derive(Deserialize, JsonSchema)]
pub struct GetCanvasParams {
    /// A canvas node, or a note whose text holds the canvas: a pimble: link
    /// (or the pimble-board: address of an embedded canvas), a node id, or a path.
    pub node: String,
    /// A picture's canvas in a note's text: its id after `#canvas=`.
    #[serde(default)]
    pub canvas: Option<String>,
    /// List only the elements with this field ("role"), or with this value of
    /// it ("role=door"); names and values match without regard to case.
    #[serde(default)]
    pub with_field: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RenderCanvasParams {
    /// The canvas node, or the note whose text holds the canvas.
    pub node: String,
    /// A picture's canvas in a note's text: its id after `#canvas=`.
    #[serde(default)]
    pub canvas: Option<String>,
    /// Only this area, in canvas units. Left out: everything the canvas shows
    /// (its page when it has one).
    #[serde(default)]
    pub area: Option<AreaSpec>,
    /// Only these elements (ids), and what is in them when one is a frame or group.
    #[serde(default)]
    pub elements: Vec<String>,
    /// The longest side of the image in pixels (default 1024, at most 2048).
    #[serde(default)]
    pub max_size: Option<u32>,
    /// Show hidden layers and hidden markup too (the canvas is not changed).
    #[serde(default)]
    pub show_all: bool,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
pub struct AreaSpec {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Colors are "#rrggbb" or "#rrggbbaa". Unset keys take the kind's default.
#[derive(Deserialize, JsonSchema, Clone, Default)]
pub struct StyleSpec {
    /// Line, outline and text color.
    #[serde(default)]
    pub stroke: Option<String>,
    /// Fill color ("none" for no fill). On a pen stroke it fills the shape
    /// the stroke outlines.
    #[serde(default)]
    pub fill: Option<String>,
    /// Line width in canvas units.
    #[serde(default)]
    pub width: Option<f32>,
    /// 0 to 1.
    #[serde(default)]
    pub opacity: Option<f32>,
    /// Text size in canvas units (default 24, 20 on sticky notes).
    #[serde(default)]
    pub font_size: Option<f32>,
    /// "left", "center" or "right".
    #[serde(default)]
    pub align: Option<String>,
    #[serde(default)]
    pub shadow: Option<bool>,
    /// Lines, arrows and connectors: "none", "start", "end" or "both".
    #[serde(default)]
    pub arrow_heads: Option<String>,
    /// "solid", "dashed" or "dotted", or a custom pattern of dash and gap
    /// lengths in canvas units ("8 4", "6 3 1 3"), as SVG's stroke-dasharray.
    #[serde(default)]
    pub dash: Option<String>,
    /// How open line ends are drawn: "butt", "round" (the default; butt when
    /// dashed) or "square".
    #[serde(default)]
    pub cap: Option<String>,
    /// How corners are drawn: "miter", "round" (the default) or "bevel".
    #[serde(default)]
    pub join: Option<String>,
    /// The miter limit for "miter" joins (default 4).
    #[serde(default)]
    pub miter: Option<f32>,
    /// rect: corner radius in canvas units (default 0).
    #[serde(default)]
    pub radius: Option<f32>,
    /// polygon: its number of sides, 3 or more (default 6).
    #[serde(default)]
    pub sides: Option<u32>,
    /// star: its number of points, 3 or more (default 5).
    #[serde(default)]
    pub points: Option<u32>,
    /// star: the inner radius as a fraction of the outer, 0 to 1 (default 0.4).
    #[serde(default)]
    pub inner: Option<f32>,
    /// Fill with a gradient instead of one color (shapes, paths, sticky
    /// notes, cards). It moves and stretches with the element.
    #[serde(default)]
    pub fill_gradient: Option<GradientSpec>,
    /// Paint the outline or line with a gradient.
    #[serde(default)]
    pub stroke_gradient: Option<GradientSpec>,
}

/// A gradient. Its handles are in the element's own box: [0,0] is its
/// top-left and [1,1] its bottom-right (before rotation), so [0,0.5] to
/// [1,0.5] runs left to right across it.
#[derive(Deserialize, JsonSchema, Clone, Default)]
pub struct GradientSpec {
    /// "linear" (the default) or "radial".
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// linear: where the first stop lies (default [0,0.5]); radial: the
    /// center (default [0.5,0.5]).
    #[serde(default)]
    pub from: Option<Vec<f32>>,
    /// linear: where the last stop lies (default [1,0.5]); radial: a point
    /// on the circle of the last stop (default [1,0.5]: the box's right edge).
    #[serde(default)]
    pub to: Option<Vec<f32>>,
    /// Two or more colors along it, in order: [{"offset": 0, "color":
    /// "#ff0000"}, {"offset": 1, "color": "#0000ff80"}]. An offset left out
    /// spreads the stops evenly.
    pub stops: Vec<StopSpec>,
}

#[derive(Deserialize, JsonSchema, Clone, Default)]
pub struct StopSpec {
    /// 0 to 1 along the gradient.
    #[serde(default)]
    pub offset: Option<f32>,
    /// "#rrggbb" or "#rrggbbaa".
    pub color: String,
}

/// One element to add. Coordinates are canvas units (one unit is one pixel of a
/// picture placed at 100%); x grows right, y grows down.
#[derive(Deserialize, JsonSchema, Clone, Default)]
pub struct ElementSpec {
    /// One of: sticky, text, rect, ellipse, diamond, polygon, star, path, frame,
    /// line, arrow, connector, pen, highlighter, step, callout, card, picture,
    /// spotlight, group. A polygon and a star fill the box x, y, w, h (style
    /// `sides`; `points` and `inner`); a rect takes style `radius`.
    pub kind: String,
    /// A name for this element within this call, so a connector's from/to, a
    /// parent or a group's members can point at it before it has an id.
    #[serde(default, rename = "ref")]
    pub name: Option<String>,
    /// Top-left corner (rect kinds, sticky, text, frame, card, picture,
    /// callout, spotlight), or a step badge's center.
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
    /// Size. Sticky notes default to 200x200 and grow to fit their text; text
    /// defaults to w 0 (no wrapping); cards to 240x56; pictures to their pixel
    /// size (give w alone to scale keeping the shape).
    #[serde(default)]
    pub w: Option<f32>,
    #[serde(default)]
    pub h: Option<f32>,
    /// line and arrow: two points [[x1,y1],[x2,y2]]. pen and highlighter: the
    /// stroke's points in order, [[x,y],...].
    #[serde(default)]
    pub points: Option<Vec<Vec<f32>>>,
    /// pen and highlighter: draw a smooth curve through the points (a few
    /// points are enough for a hill's contour or a river) instead of straight
    /// segments between them.
    #[serde(default)]
    pub smooth: Option<bool>,
    /// pen: join the last point back to the first (a closed outline; give it
    /// a fill to shade it).
    #[serde(default)]
    pub closed: Option<bool>,
    /// text: x and y are the label's center instead of its top-left corner.
    #[serde(default)]
    pub center: Option<bool>,
    /// path: its outline as SVG path data in canvas units ("M0 0 L100 0
    /// C150 0 150 80 100 80 Z"): M L H V C S Q T A Z, absolute or relative,
    /// several subpaths allowed (holes: give `rule` "evenodd"). Style it with
    /// stroke, fill, width, cap, join and dash.
    #[serde(default)]
    pub d: Option<String>,
    /// path: how overlapping subpaths fill, "nonzero" (the default) or
    /// "evenodd" (inner subpaths cut holes).
    #[serde(default)]
    pub rule: Option<String>,
    /// callout: the point its pointer touches, [x,y].
    #[serde(default)]
    pub tip: Option<Vec<f32>>,
    /// connector: the element it starts at (an id, or a ref from this call).
    /// It follows the element when that moves.
    #[serde(default)]
    pub from: Option<String>,
    /// connector: or a free point it starts at, [x,y].
    #[serde(default)]
    pub from_point: Option<Vec<f32>>,
    /// connector: which side of `from` it leaves: top, right, bottom, left
    /// (left out: the nearest side, chosen as it is drawn).
    #[serde(default)]
    pub from_side: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub to_point: Option<Vec<f32>>,
    #[serde(default)]
    pub to_side: Option<String>,
    /// card: the node it shows (a pimble: link, an id or a path); the card
    /// shows the node's live title and opens it.
    #[serde(default)]
    pub node: Option<String>,
    /// picture: a picture file on this computer (PNG, JPEG, GIF, WebP, AVIF).
    #[serde(default)]
    pub file: Option<String>,
    /// picture: or a picture already in Pimble, its pimble-blob: URL.
    #[serde(default)]
    pub src: Option<String>,
    /// The text of a sticky note, text box, shape, frame (its name), callout or
    /// connector (its label).
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub style: Option<StyleSpec>,
    /// Degrees clockwise about its center.
    #[serde(default)]
    pub rotation: Option<f32>,
    /// The layer it goes in (name or id). Left out: the call's `layer`, else the
    /// top layer that is not locked.
    #[serde(default)]
    pub layer: Option<String>,
    /// The frame or group it belongs to (an id or a ref): it moves with it.
    #[serde(default)]
    pub parent: Option<String>,
    /// group and frame: the elements (ids or refs) that belong to it.
    #[serde(default)]
    pub members: Option<Vec<String>>,
    /// Locked elements take no edits until unlocked.
    #[serde(default)]
    pub locked: Option<bool>,
    /// What it means, for the people and assistants who read the canvas: a
    /// free note ("Load-bearing wall; do not move").
    #[serde(default)]
    pub note: Option<String>,
    /// And named fields ({"role": "door", "swing": "in"}): get_canvas can list
    /// just the elements with a field (`with_field`), and search finds them.
    #[serde(default)]
    pub fields: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddElementsParams {
    /// The canvas node, or the note whose text holds the canvas.
    pub node: String,
    /// A picture's canvas in a note's text: its id after `#canvas=`.
    #[serde(default)]
    pub canvas: Option<String>,
    /// The layer they go in unless an element names its own (name or id).
    #[serde(default)]
    pub layer: Option<String>,
    /// The elements, bottom to top (each is drawn over the ones before it).
    pub elements: Vec<ElementSpec>,
}

/// A change to one element: only what is given changes.
#[derive(Deserialize, JsonSchema, Clone, Default)]
pub struct ElementChange {
    /// The element's id (get_canvas lists them).
    pub id: String,
    /// New corner or size for kinds with a rectangle (and a step's center).
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
    #[serde(default)]
    pub w: Option<f32>,
    #[serde(default)]
    pub h: Option<f32>,
    /// New points: two for a line or arrow, the whole stroke for pen and highlighter.
    #[serde(default)]
    pub points: Option<Vec<Vec<f32>>>,
    /// With new points for a stroke: a smooth curve through them, and closed.
    #[serde(default)]
    pub smooth: Option<bool>,
    #[serde(default)]
    pub closed: Option<bool>,
    /// A callout's new pointer tip, [x,y].
    #[serde(default)]
    pub tip: Option<Vec<f32>>,
    /// A path's new outline, SVG path data as add_elements takes it. Given
    /// for a rect, ellipse, diamond, polygon, star, a line without arrow
    /// heads or a pen stroke, it turns that element into a path (same id,
    /// style and place). On a path, x, y, w and h move and stretch its extent.
    #[serde(default)]
    pub d: Option<String>,
    /// A path's fill rule: "nonzero" or "evenodd".
    #[serde(default)]
    pub rule: Option<String>,
    /// A connector's new ends, as in add_elements.
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub from_point: Option<Vec<f32>>,
    #[serde(default)]
    pub from_side: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub to_point: Option<Vec<f32>>,
    #[serde(default)]
    pub to_side: Option<String>,
    /// A card's node.
    #[serde(default)]
    pub node: Option<String>,
    /// New text (edited as the smallest change, so someone typing in it keeps
    /// their typing).
    #[serde(default)]
    pub text: Option<String>,
    /// Style keys to set.
    #[serde(default)]
    pub style: Option<StyleSpec>,
    /// Style keys to clear back to the kind's default: stroke, fill, width,
    /// opacity, font_size, align, shadow, arrow_heads, dash (a custom pattern
    /// too), cap, join, miter, radius, sides, points, inner, fill_gradient,
    /// stroke_gradient (clearing fill or stroke clears its gradient too).
    #[serde(default)]
    pub clear_style: Vec<String>,
    /// Move it (and, for a frame or group, everything in it) by [dx,dy].
    #[serde(default)]
    pub move_by: Option<Vec<f32>>,
    /// Degrees clockwise about its center (replaces the rotation it had).
    #[serde(default)]
    pub rotation: Option<f32>,
    /// Another layer (name or id); it goes on top there.
    #[serde(default)]
    pub layer: Option<String>,
    /// The frame or group it belongs to (an id); "" for none.
    #[serde(default)]
    pub parent: Option<String>,
    /// Its place in its layer: "top", "bottom", "above:<id>" or "below:<id>".
    #[serde(default)]
    pub order: Option<String>,
    /// Lock or unlock it. Unlocking is the one change a locked element takes.
    #[serde(default)]
    pub locked: Option<bool>,
    /// A new note ("" empties it).
    #[serde(default)]
    pub note: Option<String>,
    /// Fields to set; a field given as null is removed. Fields not named stay.
    #[serde(default)]
    pub fields: Option<BTreeMap<String, Option<String>>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateElementsParams {
    pub node: String,
    #[serde(default)]
    pub canvas: Option<String>,
    pub changes: Vec<ElementChange>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteElementsParams {
    pub node: String,
    #[serde(default)]
    pub canvas: Option<String>,
    /// The ids of the elements to delete.
    pub ids: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CombineElementsParams {
    pub node: String,
    #[serde(default)]
    pub canvas: Option<String>,
    /// Two or more elements (ids) with outlines: shapes, paths, lines
    /// without arrow heads, pen strokes.
    pub ids: Vec<String>,
    /// "union" (everything covered), "subtract" (the bottom-most minus
    /// everything above it), "intersect" (what all of them cover) or
    /// "exclude" (what an odd number of them cover).
    pub op: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct FlipElementsParams {
    pub node: String,
    #[serde(default)]
    pub canvas: Option<String>,
    /// The elements (ids); they flip together about the center of all of them.
    pub ids: Vec<String>,
    /// "horizontal" (left becomes right) or "vertical" (top becomes bottom).
    pub direction: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ImportSvgParams {
    pub node: String,
    #[serde(default)]
    pub canvas: Option<String>,
    /// SVG markup (a whole `<svg>` document).
    #[serde(default)]
    pub svg: Option<String>,
    /// Or an .svg file on this computer.
    #[serde(default)]
    pub file: Option<String>,
    /// Where the drawing's top-left corner goes, in canvas units (default 0,0).
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
    /// x and y are where its center goes instead.
    #[serde(default)]
    pub center: bool,
    /// Scale it so its longer side is this long. Left out: one SVG pixel is
    /// one canvas unit.
    #[serde(default)]
    pub size: Option<f32>,
    /// The layer it goes in (name or id). Left out: the top vector layer that
    /// is not locked.
    #[serde(default)]
    pub layer: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PaintParams {
    /// The canvas node, or the note whose text holds the canvas.
    pub node: String,
    /// A picture's canvas in a note's text: its id after `#canvas=`.
    #[serde(default)]
    pub canvas: Option<String>,
    /// The paint layer (its name or id). Left out: the paint layer a person's
    /// brush would take (the topmost one that is shown and not locked), or a
    /// new "Paint 1" on top when there is none.
    #[serde(default)]
    pub layer: Option<String>,
    /// Highlight a picture: its element id. The strokes go into the
    /// "Highlights" paint layer just above the picture's layer (made, blended
    /// multiply, when missing), as the app's highlighter does. Leave `layer`
    /// out with it.
    #[serde(default)]
    pub highlight_picture: Option<String>,
    /// The strokes, painted in this order (later ones on top).
    pub strokes: Vec<StrokeSpec>,
}

#[derive(Deserialize, JsonSchema, Clone)]
pub struct StrokeSpec {
    /// The brush: "round" (the default), "pencil", "ink", "airbrush", "marker",
    /// "charcoal", "watercolor" or "highlighter".
    #[serde(default)]
    pub preset: Option<String>,
    /// "#rrggbb" or "#rrggbbaa". Left out: the brush's own color.
    #[serde(default)]
    pub color: Option<String>,
    /// The brush's diameter in canvas units at full pressure (each brush has
    /// its own default: round 8, pencil 2, airbrush 60...).
    #[serde(default)]
    pub size: Option<f32>,
    /// 0 to 1: the stroke's opacity where it is fully painted.
    #[serde(default)]
    pub opacity: Option<f32>,
    /// True: an eraser stroke, removing what this layer has painted under it.
    #[serde(default)]
    pub erase: bool,
    /// The points in canvas units, [[x, y], ...], each optionally with a
    /// pressure from 0 to 1 ([x, y, pressure]; left out it is 1, the
    /// brush's full size). Points about a brush's width apart or closer draw
    /// a smooth line; one point is a dab.
    pub points: Vec<Vec<f32>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddLayerParams {
    pub node: String,
    #[serde(default)]
    pub canvas: Option<String>,
    pub name: String,
    /// What it holds: "vector" (the default: elements), "paint" (brush
    /// strokes, added with the paint tool; a paint layer holds no elements)
    /// or "group" (other layers).
    #[serde(default)]
    pub kind: Option<String>,
    /// The group it goes in (a group's name or id). Left out: the top level,
    /// or the group of the layer `order` names.
    #[serde(default)]
    pub parent: Option<String>,
    /// How it is laid over what is below it: "normal" (a layer's default),
    /// "multiply", "screen", "overlay", "darken", "lighten", "color-dodge",
    /// "color-burn", "hard-light", "soft-light", "difference", "exclusion",
    /// or "pass-through" (a group's default: its layers blend as if not
    /// grouped).
    #[serde(default)]
    pub blend: Option<String>,
    /// Where it goes among the layers of its group: "top" (the default),
    /// "bottom", "above:<layer>" or "below:<layer>" (a layer's name or id;
    /// it then goes in that layer's group).
    #[serde(default)]
    pub order: Option<String>,
    /// What the layer holds, in words ("Contour lines, one every 20 m").
    #[serde(default)]
    pub note: Option<String>,
    /// Named fields ({"interval": "20 m"}).
    #[serde(default)]
    pub fields: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateLayerParams {
    pub node: String,
    #[serde(default)]
    pub canvas: Option<String>,
    /// The layer: its name or id.
    pub layer: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Shown or hidden, for everyone who opens the canvas.
    #[serde(default)]
    pub visible: Option<bool>,
    /// A locked layer's elements take no edits.
    #[serde(default)]
    pub locked: Option<bool>,
    /// 0 to 1.
    #[serde(default)]
    pub opacity: Option<f32>,
    /// A blend mode, as add_layer takes it ("pass-through" only for a group).
    #[serde(default)]
    pub blend: Option<String>,
    /// Move it into this group (a group's name or id), or "" for the top
    /// level. On top of the group unless `order` says where.
    #[serde(default)]
    pub parent: Option<String>,
    /// "top", "bottom", "above:<layer>" or "below:<layer>", among the layers
    /// of its group (above or below a layer in another group moves it there).
    #[serde(default)]
    pub order: Option<String>,
    /// A new note ("" empties it).
    #[serde(default)]
    pub note: Option<String>,
    /// Fields to set; a field given as null is removed. Fields not named stay.
    #[serde(default)]
    pub fields: Option<BTreeMap<String, Option<String>>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteLayerParams {
    pub node: String,
    #[serde(default)]
    pub canvas: Option<String>,
    /// The layer: its name or id.
    pub layer: String,
    /// Keep its elements: they go on top of the layer below in its group
    /// ("Merge Down"); for a group, every layer in it becomes one layer
    /// ("Merge Group"). Left out, they are deleted with it (a group with
    /// every layer in it).
    #[serde(default)]
    pub merge_down: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct PageSpec {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// "#rrggbb"; white when left out.
    #[serde(default)]
    pub fill: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetCanvasParams {
    pub node: String,
    #[serde(default)]
    pub canvas: Option<String>,
    /// Give the canvas a page (a rectangle it renders as, for printing or
    /// presenting; what lies outside stays). A canvas without one is infinite.
    #[serde(default)]
    pub page: Option<PageSpec>,
    /// Give it a page the size of what it shows now.
    #[serde(default)]
    pub page_fits_content: bool,
    /// Take the page away: the canvas is infinite again.
    #[serde(default)]
    pub remove_page: bool,
    /// Show or hide every mark (everything but pictures), for everyone.
    #[serde(default)]
    pub markup_visible: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateCanvasParams {
    /// The node it goes under.
    pub parent: String,
    /// Its title: "Canvas" when left out, or the picture's file name.
    #[serde(default)]
    pub title: Option<String>,
    /// Start it from a picture file on this computer: the picture at 100% in a
    /// locked layer "Picture", "Layer 1" above it to draw in.
    #[serde(default)]
    pub picture: Option<String>,
    /// Its first elements, as add_elements takes them.
    #[serde(default)]
    pub elements: Vec<ElementSpec>,
}

#[derive(Deserialize, JsonSchema)]
pub struct InsertCanvasParams {
    /// The note to put it in.
    pub node: String,
    /// It goes after the block this quote names (a few words of it, as
    /// insert_after takes). Left out: at the end.
    #[serde(default)]
    pub after: Option<String>,
    /// With `after` naming a heading: after that heading's whole section.
    #[serde(default)]
    pub after_section: bool,
    /// A picture file on this computer: the canvas holds it, locked in a layer
    /// "Picture", ready to mark up. Left out: an empty canvas to draw on.
    #[serde(default)]
    pub picture: Option<String>,
    /// Or a picture already in Pimble, its pimble-blob: URL.
    #[serde(default)]
    pub src: Option<String>,
    /// The picture's description (alt text).
    #[serde(default)]
    pub alt: Option<String>,
    /// Elements to draw on it at once, as add_elements takes them.
    #[serde(default)]
    pub elements: Vec<ElementSpec>,
}

// ── Where a canvas lives ─────────────────────────────────────────────────

/// A canvas: the node holding it and, for a picture's canvas in text, its id.
#[derive(Clone)]
struct Target {
    addr: Addr,
    node: Node,
    text_canvas: Option<String>,
}

impl Target {
    fn board(&self, doc: &NodeDoc) -> Board {
        match &self.text_canvas {
            Some(id) => Board::in_text(doc, id),
            None => Board::of_node(doc),
        }
    }

    fn what(&self) -> String {
        match &self.text_canvas {
            Some(id) => format!("canvas {id} in {} ({})", label(&self.node), self.addr.link()),
            None => format!("the canvas {} ({})", label(&self.node), self.addr.link()),
        }
    }
}

/// `#canvas=<id>` (or the whole picture URL) as the id.
fn canvas_id(given: &str) -> String {
    let given = given.trim();
    for fragment in ["#canvas=", "#board="] {
        if let Some(at) = given.find(fragment) {
            return given[at + fragment.len()..].to_string();
        }
    }
    given.to_string()
}

fn doc_of(node: &Node) -> Result<NodeDoc, String> {
    NodeDoc::load(&node.content).map_err(|e| format!("The node's document could not be read: {e}"))
}

// ── Reading values the model gives ───────────────────────────────────────

fn bad(what: impl Into<String>) -> BoardError {
    BoardError::Invalid(what.into())
}

/// A refusal as an agent reads it. `pimble_board`'s sentences for locks
/// point a person at the layers panel; an agent is pointed at the tool that
/// takes the lock off.
fn said(e: &BoardError) -> String {
    match e {
        BoardError::ElementLocked { id, layer: Some(layer) } => {
            format!("element {id} is on the locked layer \"{}\"; update_layer can unlock it", layer.name)
        }
        BoardError::ElementLocked { id, layer: None } => {
            format!("element {id} is locked; update_elements with \"locked\": false in the same change unlocks and edits it")
        }
        BoardError::LayerLocked(layer) | BoardError::LayerLockedAgainstRemoval(layer) => {
            format!("layer \"{}\" is locked; update_layer can unlock it", layer.name)
        }
        BoardError::EveryLayerLocked => "every layer of this canvas is locked (or in a locked or hidden group); update_layer can unlock one, or add_layer adds one".into(),
        BoardError::NoVectorLayer => "this canvas has no vector layer for elements; add_layer adds one".into(),
        BoardError::NotAVectorLayer { kind, .. } if kind.is_group() => "that layer is a group, which holds layers, not elements; name a layer inside it".into(),
        BoardError::NotAVectorLayer { kind, .. } => format!("that is a {kind} layer, which holds no elements; name a vector layer"),
        BoardError::NotAPaintLayer { kind, .. } => {
            format!("that is a {kind} layer; strokes go in a paint layer (name one, add one with add_layer kind \"paint\", or leave `layer` out)")
        }
        other => other.to_string(),
    }
}

/// A stroke spec as a stroke to paint (its layer filled in by the caller).
fn stroke_of(spec: &StrokeSpec, index: usize) -> Result<NewStroke, BoardError> {
    let what = format!("stroke {}", index + 1);
    let name = spec.preset.as_deref().map(str::trim).filter(|p| !p.is_empty()).unwrap_or("round");
    let preset = pimble_board::brush::preset(name).ok_or_else(|| {
        let names: Vec<&str> = pimble_board::PRESETS.iter().map(|p| p.name).collect();
        bad(format!("{what}: there is no brush \"{name}\"; the brushes are {}", names.join(", ")))
    })?;
    let color = match &spec.color {
        Some(c) => color(c, &what)?,
        None => preset.color,
    };
    let mut settings = preset.settings;
    if let Some(size) = spec.size {
        if !(size.is_finite() && size > 0.0) {
            return Err(bad(format!("{what}: the size is a number of canvas units above 0")));
        }
        settings.size = size;
    }
    if let Some(o) = spec.opacity {
        settings.opacity = o;
    }
    if spec.points.is_empty() {
        return Err(bad(format!("{what} has no points")));
    }
    let mut points = Vec::with_capacity(spec.points.len());
    for (k, pt) in spec.points.iter().enumerate() {
        let ok = matches!(pt.len(), 2 | 3) && pt.iter().all(|v| v.is_finite());
        if !ok {
            return Err(bad(format!("{what}, point {}: a point is [x, y] or [x, y, pressure]", k + 1)));
        }
        points.push([pt[0], pt[1], pt.get(2).copied().unwrap_or(1.0).clamp(0.0, 1.0), 0.0]);
    }
    let stroke = NewStroke::new(String::new(), preset, color).with_settings(settings).with_points(points);
    Ok(if spec.erase { stroke.erasing() } else { stroke })
}

fn point(v: &[f32], what: &str) -> Result<Point, BoardError> {
    match v {
        [x, y, ..] if x.is_finite() && y.is_finite() => Ok(Point::new(*x, *y)),
        _ => Err(bad(format!("{what} is a point, [x, y]"))),
    }
}

fn color(s: &str, what: &str) -> Result<Color, BoardError> {
    Color::parse(s.trim()).ok_or_else(|| bad(format!("{what} \"{s}\" is not a color; write \"#rrggbb\" or \"#rrggbbaa\"")))
}

fn style_of(spec: &StyleSpec) -> Result<Style, BoardError> {
    let pick = |v: &Option<String>, what: &str, parse: &dyn Fn(&str) -> bool, allowed: &str| -> Result<(), BoardError> {
        match v {
            Some(s) if !parse(s.trim()) => Err(bad(format!("{what} \"{s}\" is not one of {allowed}"))),
            _ => Ok(()),
        }
    };
    pick(&spec.align, "align", &|s| Align::parse(s).is_some(), "left, center, right")?;
    pick(&spec.arrow_heads, "arrow_heads", &|s| ArrowHeads::parse(s).is_some(), "none, start, end, both")?;
    pick(&spec.dash, "dash", &|s| Dash::parse(s).is_some() || parse_dash_pattern(s).is_some(), "solid, dashed, dotted, or a pattern of lengths like \"8 4\"")?;
    pick(&spec.cap, "cap", &|s| Cap::parse(s).is_some(), "butt, round, square")?;
    pick(&spec.join, "join", &|s| Join::parse(s).is_some(), "miter, round, bevel")?;
    if spec.sides.is_some_and(|n| n < 3) || spec.points.is_some_and(|n| n < 3) {
        return Err(bad("a polygon has 3 sides or more, and a star 3 points or more"));
    }
    let dash = spec.dash.as_deref().map(str::trim);
    Ok(Style {
        stroke: spec.stroke.as_deref().map(|s| color(s, "stroke")).transpose()?,
        fill: match spec.fill.as_deref() {
            Some(s) if s.trim().eq_ignore_ascii_case("none") || s.trim().eq_ignore_ascii_case("transparent") => {
                Some(Color::rgba(0, 0, 0, 0))
            }
            Some(s) => Some(color(s, "fill")?),
            None => None,
        },
        width: spec.width.map(|w| w.max(0.0)),
        opacity: spec.opacity.map(|o| o.clamp(0.0, 1.0)),
        font_size: spec.font_size.map(|f| f.max(1.0)),
        align: spec.align.as_deref().and_then(|s| Align::parse(s.trim())),
        shadow: spec.shadow,
        arrow_heads: spec.arrow_heads.as_deref().and_then(|s| ArrowHeads::parse(s.trim())),
        dash: dash.and_then(Dash::parse),
        cap: spec.cap.as_deref().and_then(|s| Cap::parse(s.trim())),
        join: spec.join.as_deref().and_then(|s| Join::parse(s.trim())),
        miter: spec.miter.filter(|m| m.is_finite()).map(|m| m.max(1.0)),
        dash_pattern: dash.filter(|s| Dash::parse(s).is_none()).and_then(parse_dash_pattern),
        radius: spec.radius.filter(|r| r.is_finite()).map(|r| r.max(0.0)),
        sides: spec.sides.map(|n| n.min(1000)),
        points: spec.points.map(|n| n.min(1000)),
        inner: spec.inner.filter(|r| r.is_finite()).map(|r| r.clamp(0.0, 1.0)),
        fill_gradient: spec.fill_gradient.as_ref().map(|g| gradient_of(g, "fill_gradient")).transpose()?,
        stroke_gradient: spec.stroke_gradient.as_ref().map(|g| gradient_of(g, "stroke_gradient")).transpose()?,
    })
}

/// A gradient as an agent writes it.
fn gradient_of(spec: &GradientSpec, what: &str) -> Result<Gradient, BoardError> {
    let kind = match spec.kind.as_deref().map(str::trim).unwrap_or("linear") {
        "" | "linear" => GradientKind::Linear,
        "radial" => GradientKind::Radial,
        other => return Err(bad(format!("{what} type \"{other}\" is not linear or radial"))),
    };
    if spec.stops.len() < 2 {
        return Err(bad(format!("{what} takes two stops or more, each {{\"offset\": 0 to 1, \"color\": \"#rrggbb\"}}")));
    }
    let last = (spec.stops.len() - 1) as f32;
    let stops = spec
        .stops
        .iter()
        .enumerate()
        .map(|(i, s)| Ok(GradientStop::new(s.offset.filter(|o| o.is_finite()).unwrap_or(i as f32 / last).clamp(0.0, 1.0), color(&s.color, "a stop's color")?)))
        .collect::<Result<Vec<_>, BoardError>>()?;
    let (from0, to0) = match kind {
        GradientKind::Linear => (Point::new(0.0, 0.5), Point::new(1.0, 0.5)),
        GradientKind::Radial => (Point::new(0.5, 0.5), Point::new(1.0, 0.5)),
    };
    let from = spec.from.as_deref().map(|p| point(p, &format!("{what} from"))).transpose()?.unwrap_or(from0);
    let to = spec.to.as_deref().map(|p| point(p, &format!("{what} to"))).transpose()?.unwrap_or(to0);
    let g = Gradient { kind, from, to, stops }.normalized();
    if !g.is_valid() {
        return Err(bad(format!("{what}: a radial gradient's `to` must lie away from its center")));
    }
    Ok(g)
}

fn gradient_text(g: &Gradient) -> String {
    let stops: Vec<String> = g.stops.iter().map(|s| format!("{} at {}", s.color.to_hex(), n2(s.offset))).collect();
    format!("{} gradient from {},{} to {},{} ({})", g.kind.as_str(), n2(g.from.x), n2(g.from.y), n2(g.to.x), n2(g.to.y), stops.join(", "))
}

/// A number with up to two decimals (box units, offsets).
fn n2(v: f32) -> String {
    let r = (v * 100.0).round() / 100.0;
    let s = format!("{r:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

/// A path's outline from SVG path data and a fill rule.
fn path_of(d: &str, rule: Option<&str>) -> Result<PathGeom, BoardError> {
    let mut p = parse_d(d).map_err(|e| bad(format!("d: {e}")))?;
    if let Some(rule) = rule.map(str::trim).filter(|r| !r.is_empty()) {
        p.rule = PathRule::parse(rule).ok_or_else(|| bad(format!("rule \"{rule}\" is not nonzero or evenodd")))?;
    }
    Ok(p)
}

fn style_key(s: &str) -> Result<StyleKey, BoardError> {
    Ok(match s.trim() {
        "stroke" => StyleKey::Stroke,
        "fill" => StyleKey::Fill,
        "width" => StyleKey::Width,
        "opacity" => StyleKey::Opacity,
        "font_size" => StyleKey::FontSize,
        "align" => StyleKey::Align,
        "shadow" => StyleKey::Shadow,
        "arrow_heads" => StyleKey::ArrowHeads,
        "dash" => StyleKey::Dash,
        "dash_pattern" => StyleKey::DashPattern,
        "cap" => StyleKey::Cap,
        "join" => StyleKey::Join,
        "miter" => StyleKey::Miter,
        "radius" => StyleKey::Radius,
        "sides" => StyleKey::Sides,
        "points" => StyleKey::Points,
        "inner" => StyleKey::Inner,
        "fill_gradient" => StyleKey::FillGradient,
        "stroke_gradient" => StyleKey::StrokeGradient,
        other => return Err(bad(format!("\"{other}\" is not a style key"))),
    })
}

fn side(s: &Option<String>) -> Result<Option<Side>, BoardError> {
    match s.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => Side::parse(s).map(Some).ok_or_else(|| bad(format!("side \"{s}\" is not top, right, bottom or left"))),
    }
}

/// Where an element's connector end touches it: its side's middle, or its center.
fn anchor_of(rect: &Rect, side: Option<Side>) -> Point {
    let c = rect.center();
    match side {
        Some(Side::Top) => Point::new(c.x, rect.y),
        Some(Side::Bottom) => Point::new(c.x, rect.bottom()),
        Some(Side::Left) => Point::new(rect.x, c.y),
        Some(Side::Right) => Point::new(rect.right(), c.y),
        None => c,
    }
}

// ── What a write needs to know (read before it, inside the same document) ──

struct LayerInfo {
    id: String,
    name: String,
    group: bool,
}

/// The canvas as it was when the write began, and the elements this write made.
struct Scene {
    layers: Vec<LayerInfo>,
    /// By id: its bounds.
    elements: HashMap<String, Rect>,
    /// Refs of this call to ids.
    refs: HashMap<String, String>,
    /// What a picture spec stood for (src, pixel size), and a card's link and title, by spec index.
    pictures: HashMap<usize, (String, u32, u32)>,
    cards: HashMap<usize, (String, String)>,
    /// What this write made or changed, for presence.
    touched: Vec<String>,
}

impl Scene {
    fn of(snapshot: &BoardSnapshot) -> Self {
        let layers = snapshot
            .layers
            .iter()
            .map(|l| LayerInfo { id: l.id.clone(), name: l.name.clone(), group: l.kind.is_group() })
            .collect::<Vec<_>>();
        let elements = snapshot.elements.iter().map(|e| (e.raw.id.clone(), e.bounds)).collect();
        Self { layers, elements, refs: HashMap::new(), pictures: HashMap::new(), cards: HashMap::new(), touched: Vec::new() }
    }

    /// Whether layer `id` is a group.
    fn is_group(&self, id: &str) -> bool {
        self.layers.iter().any(|l| l.id == id && l.group)
    }

    fn layer(&self, name: &str) -> Result<&LayerInfo, BoardError> {
        let name = name.trim();
        if let Some(l) = self.layers.iter().find(|l| l.id == name) {
            return Ok(l);
        }
        let found: Vec<&LayerInfo> = self.layers.iter().filter(|l| l.name.trim().eq_ignore_ascii_case(name)).collect();
        match found.as_slice() {
            [one] => Ok(one),
            [] => Err(bad(format!(
                "this canvas has no layer \"{name}\"; its layers are {}",
                self.layers.iter().map(|l| format!("\"{}\"", l.name)).collect::<Vec<_>>().join(", ")
            ))),
            _ => Err(bad(format!("several layers are called \"{name}\"; name one by id"))),
        }
    }

    /// The layer a new element names, by id: its own, else the call's.
    /// `None` leaves the choice to `pimble_board` (the top layer that is
    /// not locked), which also refuses a locked one.
    fn named_layer(&self, own: Option<&str>, call: Option<&str>) -> Result<Option<String>, BoardError> {
        match own.or(call).map(str::trim).filter(|s| !s.is_empty()) {
            Some(name) => Ok(Some(self.layer(name)?.id.clone())),
            None => Ok(None),
        }
    }

    /// An element id or a ref of this call, as an id.
    fn id(&self, given: &str) -> Result<String, BoardError> {
        let given = given.trim();
        if let Some(id) = self.refs.get(given) {
            return Ok(id.clone());
        }
        if self.elements.contains_key(given) {
            return Ok(given.to_string());
        }
        Err(bad(format!("there is no element \"{given}\" on this canvas (get_canvas lists the ids)")))
    }

    fn end(&self, element: &Option<String>, at: &Option<Vec<f32>>, side_of: &Option<String>, which: &str) -> Result<End, BoardError> {
        match (element.as_deref().map(str::trim).filter(|s| !s.is_empty()), at) {
            (Some(e), None) => {
                let id = self.id(e)?;
                let side = side(side_of)?;
                let last = self.elements.get(&id).map(|r| anchor_of(r, side)).unwrap_or(Point::new(0.0, 0.0));
                Ok(End::Element { id, side, last })
            }
            (None, Some(p)) => Ok(End::Point(point(p, which)?)),
            (Some(_), Some(_)) => Err(bad(format!("a connector's {which} is an element or a point, not both"))),
            (None, None) => Err(bad(format!("a connector needs its {which}: an element (`{which}`) or a point (`{which}_point`)"))),
        }
    }
}

/// The geometry a spec describes.
fn geom_of(spec: &ElementSpec, index: usize, scene: &Scene) -> Result<(Kind, Geom, Vec<[f32; 3]>), BoardError> {
    let kind = Kind::parse(spec.kind.trim());
    let xy = |what: &str| -> Result<(f32, f32), BoardError> {
        match (spec.x, spec.y) {
            (Some(x), Some(y)) => Ok((x, y)),
            _ => Err(bad(format!("a {what} needs x and y"))),
        }
    };
    let rect = |what: &str, default: Option<(f32, f32)>| -> Result<Rect, BoardError> {
        let (x, y) = xy(what)?;
        match (spec.w.or(default.map(|d| d.0)), spec.h.or(default.map(|d| d.1))) {
            (Some(w), Some(h)) => Ok(Rect::new(x, y, w, h).normalized()),
            _ => Err(bad(format!("a {what} needs w and h"))),
        }
    };
    let points = |what: &str| -> Result<Vec<Point>, BoardError> {
        spec.points.as_ref().ok_or_else(|| bad(format!("a {what} needs points")))?.iter().map(|p| point(p, "each point")).collect()
    };
    let geom = match &kind {
        Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Polygon | Kind::Star | Kind::Frame | Kind::Spotlight => {
            Geom::Rect(rect(kind.as_str(), None)?)
        }
        Kind::Path => {
            let d = spec.d.as_deref().ok_or_else(|| bad("a path needs its outline as SVG path data (`d`)"))?;
            Geom::Path(path_of(d, spec.rule.as_deref())?)
        }
        Kind::Sticky => Geom::Rect(rect("sticky note", Some((STICKY_SIZE, STICKY_SIZE)))?),
        Kind::Text if spec.center == Some(true) => {
            let (cx, cy) = xy("text label")?;
            let font = spec.style.as_ref().and_then(|s| s.font_size).unwrap_or_else(|| Style::default().resolved(&Kind::Text).font_size);
            let wrap = spec.w.filter(|w| *w > 0.0);
            let l = pimble_board::text::layout(spec.text.as_deref().unwrap_or(""), font, wrap);
            let w = wrap.unwrap_or(l.width);
            Geom::Rect(Rect::new(cx - w / 2.0, cy - l.height / 2.0, wrap.unwrap_or(0.0), 0.0))
        }
        Kind::Text => Geom::Rect(rect("text box", Some((0.0, 0.0)))?),
        Kind::Line | Kind::Arrow => match points(kind.as_str())?.as_slice() {
            [a, b] => Geom::Segment { a: *a, b: *b },
            _ => return Err(bad(format!("a {kind} takes two points, [[x1,y1],[x2,y2]]"))),
        },
        Kind::Pen | Kind::Highlighter => {
            let pts = points(kind.as_str())?;
            if pts.len() < 2 {
                return Err(bad(format!("a {kind} stroke takes at least two points")));
            }
            return Ok((kind, Geom::Stroke, stroke_points(&pts, spec.smooth == Some(true), spec.closed == Some(true))));
        }
        Kind::Step => {
            let (x, y) = xy("step")?;
            Geom::Anchor(Point::new(x, y))
        }
        Kind::Callout => Geom::Callout {
            rect: rect("callout", None)?,
            tip: point(spec.tip.as_deref().ok_or_else(|| bad("a callout needs its tip, [x,y]"))?, "tip")?,
        },
        Kind::Connector => Geom::Connector {
            from: scene.end(&spec.from, &spec.from_point, &spec.from_side, "from")?,
            to: scene.end(&spec.to, &spec.to_point, &spec.to_side, "to")?,
        },
        Kind::Card => {
            let (link, _) = scene.cards.get(&index).ok_or_else(|| bad("a card needs the node it shows (`node`)"))?;
            Geom::Card { rect: rect("card", Some(CARD_SIZE))?, link: link.clone() }
        }
        Kind::Picture => {
            let (src, pw, ph) = scene.pictures.get(&index).ok_or_else(|| bad("a picture needs a file or a pimble-blob: src"))?;
            let (x, y) = (spec.x.unwrap_or(0.0), spec.y.unwrap_or(0.0));
            let (pw_f, ph_f) = (*pw as f32, *ph as f32);
            let (w, h) = match (spec.w, spec.h) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, w * ph_f / pw_f.max(1.0)),
                (None, Some(h)) => (h * pw_f / ph_f.max(1.0), h),
                (None, None) => (pw_f, ph_f),
            };
            Geom::Picture(PictureGeom { rect: Rect::new(x, y, w, h), src: src.clone(), px_w: *pw, px_h: *ph, crop: None })
        }
        Kind::Group => Geom::None,
        Kind::Other(k) => {
            return Err(bad(format!(
                "\"{k}\" is not a kind; use sticky, text, rect, ellipse, diamond, polygon, star, path, frame, line, arrow, \
                 connector, pen, highlighter, step, callout, card, picture, spotlight or group"
            )))
        }
    };
    Ok((kind, geom, Vec::new()))
}

/// A stroke's stored points: as given, or a Catmull-Rom curve through them
/// (eight steps a segment, so it reads as a curve at any zoom the app shows
/// it at), closed back to the first point when asked. Pressure 0.5: the
/// stroke's own width all along.
fn stroke_points(pts: &[Point], smooth: bool, closed: bool) -> Vec<[f32; 3]> {
    let mut out: Vec<Point> = Vec::new();
    if smooth && pts.len() >= 3 {
        let n = pts.len();
        let at = |i: isize| -> Point {
            if closed {
                pts[i.rem_euclid(n as isize) as usize]
            } else {
                pts[i.clamp(0, n as isize - 1) as usize]
            }
        };
        let segments = if closed { n } else { n - 1 };
        for i in 0..segments as isize {
            let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
            for k in 0..8 {
                let t = k as f32 / 8.0;
                let (t2, t3) = (t * t, t * t * t);
                let c = |a: f32, b: f32, c: f32, d: f32| {
                    0.5 * (2.0 * b + (-a + c) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2 + (-a + 3.0 * b - 3.0 * c + d) * t3)
                };
                out.push(Point::new(c(p0.x, p1.x, p2.x, p3.x), c(p0.y, p1.y, p2.y, p3.y)));
            }
        }
        out.push(if closed { pts[0] } else { pts[n - 1] });
    } else {
        out.extend_from_slice(pts);
        if closed && pts.first() != pts.last() {
            out.push(pts[0]);
        }
    }
    out.iter().map(|p| [p.x, p.y, 0.5]).collect()
}

/// The rectangle a new element takes, for connectors made in the same call.
fn rect_of_geom(geom: &Geom, points: &[[f32; 3]]) -> Rect {
    match geom {
        Geom::Rect(r) | Geom::Callout { rect: r, .. } | Geom::Card { rect: r, .. } => *r,
        Geom::Picture(p) => p.rect,
        Geom::Segment { a, b } => Rect::from_points(*a, *b),
        Geom::Anchor(p) => Rect::new(p.x - 12.0, p.y - 12.0, 24.0, 24.0),
        Geom::Stroke => points
            .iter()
            .map(|p| Rect::new(p[0], p[1], 0.0, 0.0))
            .reduce(|a, b| a.union(&b))
            .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0)),
        Geom::Connector { from, to } => Rect::from_points(from.last_point(), to.last_point()),
        Geom::Path(p) => p.bounds().unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0)),
        Geom::None => Rect::new(0.0, 0.0, 0.0, 0.0),
    }
}

/// Add every spec, bottom to top. Connectors go in after the elements they
/// join, so a connector may name an element later in the list; membership and
/// locks are written last.
fn add_specs(t: &mut BoardTxn<'_, '_>, scene: &mut Scene, specs: &[ElementSpec], call_layer: Option<&str>) -> Result<Vec<String>, BoardError> {
    let mut ids: Vec<Option<String>> = vec![None; specs.len()];
    let mut seen_refs = HashSet::new();
    for spec in specs {
        if let Some(name) = spec.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if !seen_refs.insert(name.to_string()) {
                return Err(bad(format!("two elements in this call have the ref \"{name}\"")));
            }
        }
    }
    // Ids are minted when an element is added; refs to elements added later
    // (a connector before its ends) are resolved in a second pass.
    let order: Vec<usize> = (0..specs.len())
        .filter(|&i| Kind::parse(specs[i].kind.trim()) != Kind::Connector)
        .chain((0..specs.len()).filter(|&i| Kind::parse(specs[i].kind.trim()) == Kind::Connector))
        .collect();
    for i in order {
        let spec = &specs[i];
        let (kind, geom, points) = geom_of(spec, i, scene)?;
        let mut element = NewElement::new(kind.clone(), geom.clone()).with_points(points.clone());
        element.layer = scene.named_layer(spec.layer.as_deref(), call_layer)?;
        if let Some(text) = &spec.text {
            if matches!(kind, Kind::Pen | Kind::Highlighter | Kind::Picture | Kind::Group | Kind::Step | Kind::Line | Kind::Arrow | Kind::Path) {
                return Err(bad(format!("a {kind} holds no text")));
            }
            element = element.with_text(text.clone());
        } else if let (Kind::Card, Some((_, title))) = (&kind, scene.cards.get(&i)) {
            // What shows where the node cannot be read; the live title wins.
            element = element.with_text(title.clone());
        }
        if let Some(style) = &spec.style {
            element = element.with_style(style_of(style)?);
        }
        if let Some(rot) = spec.rotation.filter(|r| *r != 0.0) {
            element = element.with_transform(Transform { rot, ..Transform::IDENTITY });
        }
        let id = t.add_element(element)?;
        scene.elements.insert(id.clone(), rect_of_geom(&geom, &points));
        if let Some(name) = spec.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            scene.refs.insert(name.to_string(), id.clone());
        }
        ids[i] = Some(id.clone());
        scene.touched.push(id);
    }
    let ids: Vec<String> = ids.into_iter().map(|id| id.expect("every spec was added")).collect();
    for (spec, id) in specs.iter().zip(&ids) {
        if let Some(parent) = spec.parent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let parent = scene.id(parent)?;
            t.set_parent(id, Some(&parent))?;
        }
        for member in spec.members.iter().flatten() {
            let member = scene.id(member)?;
            t.set_parent(&member, Some(id))?;
        }
    }
    for (spec, id) in specs.iter().zip(&ids) {
        if let Some(note) = spec.note.as_deref().filter(|n| !n.trim().is_empty()) {
            t.set_note(id, note)?;
        }
        for (name, value) in spec.fields.iter().flatten() {
            t.set_field(id, name, Some(value))?;
        }
    }
    for (spec, id) in specs.iter().zip(&ids) {
        if spec.locked == Some(true) {
            t.set_locked(id, true)?;
        }
    }
    Ok(ids)
}

fn placement(order: &str, resolve: &dyn Fn(&str) -> Result<String, BoardError>) -> Result<Placement, BoardError> {
    let order = order.trim();
    Ok(match order {
        "top" | "" => Placement::Top,
        "bottom" => Placement::Bottom,
        _ => match order.split_once(':') {
            Some(("above", other)) => Placement::Above(resolve(other)?),
            Some(("below", other)) => Placement::Below(resolve(other)?),
            _ => return Err(bad(format!("order \"{order}\" is not top, bottom, above:<…> or below:<…>"))),
        },
    })
}

/// Whether a change names anything of an element's geometry.
fn change_names_geometry(c: &ElementChange) -> bool {
    [c.x, c.y, c.w, c.h].iter().any(Option::is_some)
        || c.points.is_some()
        || c.tip.is_some()
        || [&c.from, &c.from_side, &c.to, &c.to_side, &c.node].iter().any(|v| v.is_some())
        || c.from_point.is_some()
        || c.to_point.is_some()
        || c.d.is_some()
        || c.rule.is_some()
}

/// Apply one change to an element. Locks are `pimble_board`'s to judge: a
/// locked element, or one on a locked layer, takes a change of its note and
/// fields and nothing else, unless the change unlocks it ("locked": false,
/// written first) and its layer is not locked.
fn change_element(t: &mut BoardTxn<'_, '_>, scene: &mut Scene, snapshot: &BoardSnapshot, change: &ElementChange, index: usize) -> Result<(), BoardError> {
    let id = change.id.trim().to_string();
    let element = snapshot.get(&id).ok_or_else(|| bad(format!("there is no element \"{id}\" on this canvas (get_canvas lists the ids)")))?;
    if element.raw.locked && change.locked == Some(false) {
        t.set_locked(&id, false)?;
    }
    // New path data for a shape, a line or a pen stroke makes it a path.
    let converted = match (&change.d, &element.raw.geom) {
        (Some(_), Geom::Path(_)) | (None, _) => None,
        (Some(_), _) => Some(t.convert_to_path(&id).map_err(|e| match e {
            BoardError::Invalid(_) => bad(format!("a {} cannot take path data; only shapes, lines without arrow heads and pen strokes turn into paths", element.raw.kind)),
            other => other,
        })?),
    };
    let raw = &element.raw;
    let raw_geom = match &converted {
        Some(p) => Geom::Path(p.clone()),
        None => raw.geom.clone(),
    };
    // Geometry, merged into what it has.
    let r = |r: &Rect| Rect::new(change.x.unwrap_or(r.x), change.y.unwrap_or(r.y), change.w.unwrap_or(r.w), change.h.unwrap_or(r.h)).normalized();
    let new_points = || -> Result<Option<Vec<Point>>, BoardError> {
        change.points.as_ref().map(|ps| ps.iter().map(|p| point(p, "each point")).collect()).transpose()
    };
    let geom = if !change_names_geometry(change) {
        None
    } else {
        match &raw_geom {
            Geom::Path(p) => {
                let mut p = match change.d.as_deref() {
                    Some(d) => path_of(d, None)?,
                    None => p.clone(),
                };
                if [change.x, change.y, change.w, change.h].iter().any(Option::is_some) {
                    if let Some(b) = p.bounds() {
                        p = p.rescaled(&b, &r(&b));
                    }
                }
                if let Some(rule) = change.rule.as_deref() {
                    p.rule = path_of("M0 0 L1 0", Some(rule))?.rule;
                } else if change.d.is_some() {
                    if let Geom::Path(old) = &raw_geom {
                        p.rule = old.rule;
                    }
                }
                Some(Geom::Path(p))
            }
            Geom::Rect(rect) => Some(Geom::Rect(r(rect))),
            Geom::Picture(p) => Some(Geom::Picture(PictureGeom { rect: r(&p.rect), ..p.clone() })),
            Geom::Callout { rect, tip } => Some(Geom::Callout {
                rect: r(rect),
                tip: change.tip.as_deref().map(|p| point(p, "tip")).transpose()?.unwrap_or(*tip),
            }),
            Geom::Card { rect, link } => Some(Geom::Card {
                rect: r(rect),
                link: scene.cards.get(&index).map(|(l, _)| l.clone()).unwrap_or_else(|| link.clone()),
            }),
            Geom::Anchor(p) => Some(Geom::Anchor(Point::new(change.x.unwrap_or(p.x), change.y.unwrap_or(p.y)))),
            Geom::Segment { a, b } => match new_points()?.as_deref() {
                None => Some(Geom::Segment { a: *a, b: *b }),
                Some([a, b]) => Some(Geom::Segment { a: *a, b: *b }),
                Some(_) => return Err(bad("a line or arrow takes two points")),
            },
            Geom::Connector { from, to } => {
                let end = |el: &Option<String>, pt: &Option<Vec<f32>>, sd: &Option<String>, old: &End, which: &str| -> Result<End, BoardError> {
                    if el.is_none() && pt.is_none() {
                        if sd.is_some() {
                            if let End::Element { id, last, .. } = old {
                                return Ok(End::Element { id: id.clone(), side: side(sd)?, last: *last });
                            }
                        }
                        return Ok(old.clone());
                    }
                    scene.end(el, pt, sd, which)
                };
                Some(Geom::Connector {
                    from: end(&change.from, &change.from_point, &change.from_side, from, "from")?,
                    to: end(&change.to, &change.to_point, &change.to_side, to, "to")?,
                })
            }
            Geom::Stroke => {
                if let Some(points) = new_points()? {
                    if points.len() < 2 {
                        return Err(bad("a stroke takes at least two points"));
                    }
                    t.replace_points(&id, &stroke_points(&points, change.smooth == Some(true), change.closed == Some(true)))?;
                }
                None
            }
            Geom::None => None,
        }
    };
    if let Some(geom) = geom.filter(|g| *g != raw_geom) {
        t.set_geom(&id, &geom)?;
    }
    if let Some(text) = &change.text {
        t.set_text(&id, text)?;
    }
    if let Some(style) = &change.style {
        let style = style_of(style)?;
        // A preset dash replaces a custom pattern (which would win over it).
        if style.dash.is_some() && raw.style.dash_pattern.is_some() {
            t.clear_style(&id, StyleKey::DashPattern)?;
        }
        // A solid color (or "none") replaces a gradient (which would win).
        if style.fill.is_some() && style.fill_gradient.is_none() && raw.style.fill_gradient.is_some() {
            t.clear_style(&id, StyleKey::FillGradient)?;
        }
        if style.stroke.is_some() && style.stroke_gradient.is_none() && raw.style.stroke_gradient.is_some() {
            t.clear_style(&id, StyleKey::StrokeGradient)?;
        }
        t.set_style(&id, &style)?;
    }
    for key in &change.clear_style {
        t.clear_style(&id, style_key(key)?)?;
    }
    if change.move_by.is_some() || change.rotation.is_some() {
        let mut transform = raw.transform;
        if let Some(by) = &change.move_by {
            let by = point(by, "move_by")?;
            transform.tx += by.x;
            transform.ty += by.y;
        }
        if let Some(rot) = change.rotation {
            transform.rot = rot;
        }
        t.set_transform(&id, &transform)?;
    }
    if let Some(layer) = change.layer.as_deref() {
        let layer = scene.layer(layer)?.id.clone();
        t.move_to_layer(std::slice::from_ref(&id), &layer)?;
    }
    if let Some(parent) = change.parent.as_deref() {
        let parent = parent.trim();
        if parent.is_empty() {
            t.set_parent(&id, None)?;
        } else {
            let parent = scene.id(parent)?;
            t.set_parent(&id, Some(&parent))?;
        }
    }
    if let Some(order) = change.order.as_deref() {
        let at = placement(order, &|other| scene.id(other))?;
        t.move_element(&id, at)?;
    }
    if let Some(note) = &change.note {
        t.set_note(&id, note)?;
    }
    for (name, value) in change.fields.iter().flatten() {
        t.set_field(&id, name, value.as_deref())?;
    }
    if change.locked == Some(true) {
        t.set_locked(&id, true)?;
    }
    scene.touched.push(id);
    Ok(())
}

// ── Describing a canvas ──────────────────────────────────────────────────

/// A number as a person writes it: no trailing ".0".
fn n(v: f32) -> String {
    let r = (v * 10.0).round() / 10.0;
    if r == r.trunc() {
        format!("{}", r as i64)
    } else {
        format!("{r:.1}")
    }
}

fn rect_text(r: &Rect) -> String {
    format!("at {},{} size {}x{}", n(r.x), n(r.y), n(r.w), n(r.h))
}

fn end_text(end: &End) -> String {
    match end {
        End::Element { id, side: Some(side), .. } => format!("{id} ({})", side.as_str()),
        End::Element { id, side: None, .. } => id.clone(),
        End::Point(p) => format!("{},{}", n(p.x), n(p.y)),
    }
}

fn style_text(style: &Style) -> Vec<String> {
    let mut out = Vec::new();
    match (&style.stroke_gradient, style.stroke) {
        (Some(g), _) => out.push(format!("stroke {}", gradient_text(g))),
        (None, Some(c)) => out.push(format!("stroke {}", c.to_hex())),
        (None, None) => {}
    }
    match (&style.fill_gradient, style.fill) {
        (Some(g), _) => out.push(format!("fill {}", gradient_text(g))),
        (None, Some(c)) => out.push(if c.a == 0 { "no fill".to_string() } else { format!("fill {}", c.to_hex()) }),
        (None, None) => {}
    }
    if let Some(w) = style.width {
        out.push(format!("width {}", n(w)));
    }
    if let Some(o) = style.opacity {
        out.push(format!("opacity {}", n(o)));
    }
    if let Some(f) = style.font_size {
        out.push(format!("font {}", n(f)));
    }
    if let Some(a) = style.align {
        out.push(format!("align {}", a.as_str()));
    }
    if let Some(s) = style.shadow {
        out.push(if s { "shadow".into() } else { "no shadow".into() });
    }
    if let Some(h) = style.arrow_heads {
        out.push(format!("heads {}", h.as_str()));
    }
    if let Some(d) = style.dash {
        out.push(d.as_str().to_string());
    }
    if let Some(p) = &style.dash_pattern {
        out.push(format!("dash \"{}\"", dash_pattern_text(p)));
    }
    if let Some(c) = style.cap {
        out.push(format!("cap {}", c.as_str()));
    }
    if let Some(j) = style.join {
        out.push(format!("join {}", j.as_str()));
    }
    if let Some(m) = style.miter {
        out.push(format!("miter {}", n(m)));
    }
    if let Some(r) = style.radius {
        out.push(format!("radius {}", n(r)));
    }
    if let Some(v) = style.sides {
        out.push(format!("{v} sides"));
    }
    if let Some(v) = style.points {
        out.push(format!("{v} points"));
    }
    if let Some(v) = style.inner {
        out.push(format!("inner {}", n(v)));
    }
    out
}

/// One line per element, grouped by layer, bottom to top: everything an
/// assistant needs to name, place and change it.
/// Its note and fields, indented under the line that names it.
fn meaning_text(note: Option<&str>, fields: &[(String, String)]) -> String {
    let mut out = String::new();
    if let Some(note) = note.filter(|n| !n.trim().is_empty()) {
        out.push_str(&format!("  note: {}\n", note.trim().replace('\n', "\n        ")));
    }
    if !fields.is_empty() {
        let fields: Vec<String> = fields.iter().map(|(n, v)| format!("{n} = {v}")).collect();
        out.push_str(&format!("  fields: {}\n", fields.join("; ")));
    }
    out
}

/// `word` with its first letter in capitals ("paint" -> "Paint").
fn capitalized(word: &str) -> String {
    let mut c = word.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// A layer kind as an agent writes it.
fn layer_kind(kind: Option<&str>) -> Result<LayerKind, BoardError> {
    match kind.map(str::trim).unwrap_or("vector") {
        "" | "vector" => Ok(LayerKind::Vector),
        "paint" => Ok(LayerKind::Paint),
        "group" => Ok(LayerKind::Group),
        other => Err(bad(format!("kind \"{other}\" is not vector, paint or group"))),
    }
}

/// A blend mode as an agent writes it.
fn blend_mode(blend: &str) -> Result<Blend, BoardError> {
    Blend::parse(blend.trim()).ok_or_else(|| {
        let all: Vec<&str> = Blend::ALL.iter().map(|b| b.as_str()).collect();
        bad(format!("blend \"{}\" is not one of {}", blend.trim(), all.join(", ")))
    })
}

/// Whether `fields` has the field `wanted` names ("role", or "role=door").
fn has_field(fields: &[(String, String)], wanted: &str) -> bool {
    let (name, value) = match wanted.split_once('=') {
        Some((n, v)) => (n.trim(), Some(v.trim())),
        None => (wanted.trim(), None),
    };
    fields
        .iter()
        .any(|(n, v)| n.trim().eq_ignore_ascii_case(name) && value.is_none_or(|want| v.trim().eq_ignore_ascii_case(want)))
}

/// What a paint layer holds, in a line or two: how many operations of each
/// kind, the brushes and colors, who painted, and where it paints. Never the
/// points.
fn paint_summary(snapshot: &BoardSnapshot, layer: &str) -> String {
    let ops = snapshot.paint_ops(layer);
    if ops.is_empty() {
        return "nothing painted yet\n".to_string();
    }
    let (mut strokes, mut erasing, mut pictures, mut other) = (0, 0, 0, 0);
    let mut brushes: BTreeMap<String, usize> = BTreeMap::new();
    let mut colors: Vec<String> = Vec::new();
    let mut authors: Vec<String> = Vec::new();
    for op in ops {
        match &op.raw.body {
            PaintBody::Stroke(s) if s.blend == StrokeBlend::Erase => erasing += 1,
            PaintBody::Stroke(s) => {
                strokes += 1;
                *brushes.entry(s.preset.clone()).or_default() += 1;
                let hex = s.color.to_hex();
                if !colors.contains(&hex) {
                    colors.push(hex);
                }
            }
            PaintBody::Pixels(_) => pictures += 1,
            PaintBody::Other(_) => other += 1,
        }
        if let Some(a) = &op.raw.author {
            if !authors.contains(&a.name) {
                authors.push(a.name.clone());
            }
        }
    }
    let mut parts = vec![format!("{strokes} brush stroke{}", if strokes == 1 { "" } else { "s" })];
    if erasing > 0 {
        parts.push(format!("{erasing} eraser stroke{}", if erasing == 1 { "" } else { "s" }));
    }
    if pictures > 0 {
        parts.push(format!("{pictures} picture{}", if pictures == 1 { "" } else { "s" }));
    }
    if other > 0 {
        parts.push(format!("{other} operation{} of a later version", if other == 1 { "" } else { "s" }));
    }
    let mut line = parts.join(", ");
    if !brushes.is_empty() {
        line.push_str(&format!(" · brushes {}", brushes.iter().map(|(b, n)| format!("{b} x{n}")).collect::<Vec<_>>().join(", ")));
    }
    if !colors.is_empty() {
        let shown: Vec<&str> = colors.iter().take(8).map(String::as_str).collect();
        line.push_str(&format!(" · colors {}{}", shown.join(", "), if colors.len() > 8 { ", ..." } else { "" }));
    }
    if let Some(b) = snapshot.paint_bounds(layer) {
        line.push_str(&format!(" · paints the area {}", rect_text(&b)));
    }
    if !authors.is_empty() {
        line.push_str(&format!(" · by {}", authors.join(", ")));
    }
    line.push('\n');
    line
}

#[cfg(test)]
fn describe(snapshot: &BoardSnapshot, cards: &HashMap<String, String>) -> String {
    describe_where(snapshot, cards, None)
}

fn describe_where(snapshot: &BoardSnapshot, cards: &HashMap<String, String>, with_field: Option<&str>) -> String {
    let mut out = String::new();
    match &snapshot.page {
        Some(page) => out.push_str(&format!(
            "page: {}{}\n",
            rect_text(&page.rect),
            page.fill.map(|f| format!(", fill {}", f.to_hex())).unwrap_or_default()
        )),
        None => out.push_str("page: none (an infinite canvas)\n"),
    }
    match snapshot.visible_extent() {
        Some(r) => out.push_str(&format!("what shows: {}\n", rect_text(&r))),
        None => out.push_str("what shows: nothing\n"),
    }
    if !snapshot.markup_visible {
        out.push_str("markup: hidden (only pictures show; set_canvas markup_visible shows it)\n");
    }
    out.push_str(&format!("elements: {}\n", snapshot.elements.len()));
    let mut listed = 0;
    for layer in snapshot.layers.iter().rev() {
        let mut flags = Vec::new();
        if let Some(group) = layer.parent.as_deref().and_then(|p| snapshot.layer(p)) {
            flags.push(format!("in group \"{}\"", group.name));
        }
        if layer.blend != layer.kind.default_blend() {
            flags.push(format!("blend {}", layer.blend));
        }
        if !layer.visible {
            flags.push("hidden".to_string());
        } else if !layer.effective_visible {
            flags.push("hidden by its group".to_string());
        }
        if layer.locked {
            flags.push("locked".to_string());
        } else if layer.effective_locked {
            flags.push("locked by its group".to_string());
        }
        if layer.opacity < 1.0 {
            flags.push(format!("opacity {}", n(layer.opacity)));
        }
        let flags = if flags.is_empty() { String::new() } else { format!(", {}", flags.join(", ")) };
        let heading = "#".repeat(2 + layer.depth.min(4));
        match &layer.kind {
            LayerKind::Group => {
                let held = snapshot.children_of(Some(&layer.id)).len();
                out.push_str(&format!(
                    "\n{heading} Group \"{}\" ({}){flags}, {held} layer{}, top first\n",
                    layer.name,
                    layer.id,
                    if held == 1 { "" } else { "s" }
                ));
                out.push_str(&meaning_text(layer.note.as_deref(), &layer.fields));
                continue;
            }
            LayerKind::Vector => out.push_str(&format!(
                "\n{heading} Layer \"{}\" ({}){flags}, {} element{}, top first\n",
                layer.name,
                layer.id,
                layer.count,
                if layer.count == 1 { "" } else { "s" }
            )),
            LayerKind::Paint => {
                out.push_str(&format!("\n{heading} Paint layer \"{}\" ({}){flags}\n", layer.name, layer.id));
                out.push_str(&paint_summary(snapshot, &layer.id));
            }
            other => out.push_str(&format!("\n{heading} {} layer \"{}\" ({}){flags}\n", capitalized(other.as_str()), layer.name, layer.id)),
        }
        out.push_str(&meaning_text(layer.note.as_deref(), &layer.fields));
        for e in snapshot.elements.iter().rev().filter(|e| e.layer == layer.id && with_field.is_none_or(|w| has_field(&e.raw.fields, w))) {
            if listed == LIST_LIMIT {
                out.push_str(&format!("... and more; render_canvas shows the whole canvas, and these {LIST_LIMIT} are the topmost.\n"));
                return out;
            }
            listed += 1;
            let raw = &e.raw;
            let mut line = format!("- {} {}", raw.id, raw.kind);
            if let Some(text) = raw.text.as_deref().filter(|t| !t.trim().is_empty()) {
                let shown: String = text.chars().take(300).collect();
                let more = if text.chars().count() > 300 { "…" } else { "" };
                line.push_str(&format!(" \"{}{more}\"", shown.replace('\n', "\\n")));
            }
            let geom = match &raw.geom {
                // Text and sticky notes take the room their text needs.
                Geom::Rect(r) if raw.kind == Kind::Text => match e.text_box {
                    Some(tb) => format!("{} (its text)", rect_text(&tb)),
                    None => rect_text(r),
                },
                Geom::Rect(r) if raw.kind == Kind::Sticky && e.grown.is_some_and(|g| g.h > r.h) => {
                    format!("{}, grown to {}x{} by its text", rect_text(r), n(r.w), n(e.grown.map_or(r.h, |g| g.h)))
                }
                Geom::Rect(r) => rect_text(r),
                Geom::Picture(p) => format!(
                    "{}, picture {} ({}x{} px){}",
                    rect_text(&p.rect),
                    p.src,
                    p.px_w,
                    p.px_h,
                    p.crop.map(|c| format!(", cropped to {}", rect_text(&c))).unwrap_or_default()
                ),
                Geom::Segment { a, b } => format!("from {},{} to {},{}", n(a.x), n(a.y), n(b.x), n(b.y)),
                Geom::Anchor(p) => format!(
                    "at {},{}{}",
                    n(p.x),
                    n(p.y),
                    e.step_number.map(|s| format!(", number {s}")).unwrap_or_default()
                ),
                Geom::Callout { rect, tip } => format!("{}, pointing at {},{}", rect_text(rect), n(tip.x), n(tip.y)),
                Geom::Connector { from, to } => format!("from {} to {}", end_text(from), end_text(to)),
                Geom::Card { rect, link } => format!(
                    "{}, shows {}{}",
                    rect_text(rect),
                    cards.get(link).map(|t| format!("\"{t}\" ")).unwrap_or_default(),
                    link
                ),
                Geom::Stroke => format!("{} points within {}", raw.points.len(), rect_text(&e.bounds)),
                Geom::Path(p) => {
                    let d = to_d(p);
                    let rule = if p.rule == PathRule::EvenOdd { ", rule evenodd" } else { "" };
                    if d.len() > PATH_TEXT_LIMIT {
                        format!(
                            "{} anchors within {}{rule}, d too long to list (render_canvas shows it)",
                            p.anchor_count(),
                            rect_text(&p.bounds().unwrap_or_default())
                        )
                    } else {
                        format!("d=\"{d}\"{rule}")
                    }
                }
                Geom::None => format!("holding what is within {}", rect_text(&e.bounds)),
            };
            line.push_str(&format!(" {geom}"));
            let t = &raw.transform;
            if t.tx != 0.0 || t.ty != 0.0 {
                line.push_str(&format!(", moved by {},{}", n(t.tx), n(t.ty)));
            }
            if t.rot != 0.0 {
                line.push_str(&format!(", rotated {}°", n(t.rot)));
            }
            if t.sx != 1.0 || t.sy != 1.0 {
                line.push_str(&format!(", scaled {}x{}", n(t.sx), n(t.sy)));
            }
            let mut style = style_text(&raw.style);
            match raw.kind {
                Kind::Polygon if raw.style.sides.is_none() => style.insert(0, format!("{} sides", e.style.sides)),
                Kind::Star if raw.style.points.is_none() => style.insert(0, format!("{} points", e.style.points)),
                _ => {}
            }
            if !style.is_empty() {
                line.push_str(&format!(" · {}", style.join(", ")));
            }
            if let Some(parent) = &e.parent {
                line.push_str(&format!(" · in {parent}"));
            }
            if raw.locked {
                line.push_str(" · locked");
            }
            if let Some(author) = &raw.author {
                line.push_str(&format!(" · by {}", author.name));
            }
            out.push_str(&line);
            out.push('\n');
            out.push_str(&meaning_text(raw.note.as_deref(), &raw.fields));
        }
    }
    out
}

// ── The tools' shared steps ──────────────────────────────────────────────

impl Pimble {
    /// Who this agent's elements are stamped with: its id and name, as the
    /// server knows them (none when no account is signed in, as for a person).
    async fn board_author(&self) -> Option<Author> {
        self.api.me().await.map(|me| me.board_author("AI agent"))
    }

    /// The canvas a tool was given.
    async fn canvas(&self, node: &str, canvas: Option<&str>) -> Result<Target, String> {
        let node = node.trim();
        // An embedded canvas node's address, as get_node shows it in text.
        let node = match node.strip_prefix("pimble-board:") {
            Some(rest) if rest.contains('/') => format!("pimble:{}", rest.split('#').next().unwrap_or(rest)),
            _ => node.to_string(),
        };
        let addr = self.api.resolve(&node).await?;
        let found = self.api.node(addr).await?;
        let canvas = canvas.map(canvas_id).filter(|c| !c.is_empty());
        if found.node_type == node_types::BOARD {
            if canvas.is_some() {
                return Err(format!("{} is a canvas node; leave `canvas` out to name it.", label(&found)));
            }
            return Ok(Target { addr, node: found, text_canvas: None });
        }
        let doc = doc_of(&found)?;
        let held = doc.held_canvases();
        match canvas {
            Some(id) if held.contains(&id) => Ok(Target { addr, node: found, text_canvas: Some(id) }),
            Some(id) => Err(format!(
                "{} holds no canvas {id}.{}",
                label(&found),
                if held.is_empty() { " It has no canvases in its text; insert_canvas puts one in.".to_string() } else { format!(" Its canvases: {}.", sorted(&held).join(", ")) }
            )),
            None if held.is_empty() => Err(format!(
                "{} is not a canvas and holds none in its text. create_canvas makes a canvas node; insert_canvas puts one in a note's text.",
                label(&found)
            )),
            None => Err(format!(
                "{} is a note; name which canvas in its text with `canvas`: {}.",
                label(&found),
                sorted(&held).join(", ")
            )),
        }
    }

    /// The titles of the nodes the canvas's cards show, by link.
    async fn card_titles(&self, snapshot: &BoardSnapshot) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for e in &snapshot.elements {
            if let Geom::Card { link, .. } = &e.raw.geom {
                if out.contains_key(link) {
                    continue;
                }
                if let Some(url) = PimbleUrl::parse(link) {
                    if let Ok(node) = self.api.node(Addr { store: url.store, node: url.node }).await {
                        out.insert(link.clone(), label(&node));
                    }
                }
            }
        }
        out
    }

    /// Store a picture file as a blob of `addr`'s store (as a write of
    /// `addr`), fitted as the app fits one: its URL and pixel size.
    async fn store_picture(&self, addr: Addr, file: &str) -> Result<(String, u32, u32), String> {
        let path = std::path::Path::new(file.trim());
        let bytes = std::fs::read(path).map_err(|e| format!("The picture {} could not be read: {e}", path.display()))?;
        let fitted = pimble_image::fit(bytes).map_err(|e| e.to_string())?;
        let picture = Picture::decode(&fitted.bytes).map_err(|e| format!("{} is not a picture Pimble can show: {e}", path.display()))?;
        let url = self.api.put_blob(addr, fitted.mime.as_str(), &fitted.bytes).await?;
        Ok((url.to_string(), picture.width(), picture.height()))
    }

    /// A picture already in Pimble: its URL and pixel size.
    async fn known_picture(&self, src: &str) -> Result<(String, u32, u32), String> {
        let src = src.trim().split('#').next().unwrap_or_default();
        let url = BlobUrl::parse(src).ok_or_else(|| format!("\"{src}\" is not a pimble-blob: URL."))?;
        let (_, bytes) = self.api.get_blob(url.store, url.blob).await?;
        let picture = Picture::decode(&bytes).map_err(|e| format!("{src} is not a picture Pimble can show: {e}"))?;
        Ok((url.to_string(), picture.width(), picture.height()))
    }

    /// Everything a list of specs needs from outside the document: pictures
    /// stored, cards' nodes found. Picture blobs are written as writes of `owner`.
    async fn prepare(
        &self,
        owner: Addr,
        specs: &[ElementSpec],
    ) -> Result<Prepared, String> {
        let mut pictures = HashMap::new();
        let mut cards = HashMap::new();
        for (i, spec) in specs.iter().enumerate() {
            let kind = Kind::parse(spec.kind.trim());
            if kind == Kind::Picture {
                let picture = match (&spec.file, &spec.src) {
                    (Some(file), None) => self.store_picture(owner, file).await?,
                    (None, Some(src)) => self.known_picture(src).await?,
                    _ => return Err(format!("Element {} (a picture) takes a file or a src, one of them.", i + 1)),
                };
                pictures.insert(i, picture);
            }
            if let Some(node) = &spec.node {
                cards.insert(i, self.card_target(node).await?);
            } else if kind == Kind::Card {
                return Err(format!("Element {} (a card) needs the node it shows (`node`).", i + 1));
            }
        }
        Ok((pictures, cards))
    }

    async fn card_target(&self, node: &str) -> Result<(String, String), String> {
        let addr = self.api.resolve(node).await?;
        let found = self.api.node(addr).await?;
        Ok((addr.link(), label(&found)))
    }

    /// One write to a canvas: `write` runs in one board transaction over the
    /// canvas as it is in the document fetched for the write, and what it
    /// changed goes through `applyEdit`. Then the agent is on the canvas with
    /// what it touched selected. Answers what `write` answered.
    async fn canvas_edit<R: Send + 'static>(
        &self,
        target: &Target,
        prepared: Prepared,
        write: impl FnOnce(&mut BoardTxn<'_, '_>, &mut Scene, &BoardSnapshot) -> Result<R, BoardError> + Send,
    ) -> Result<R, String> {
        let author = self.board_author().await;
        let now = now();
        let mut answer: Option<(R, Vec<String>)> = None;
        let answer_ref = &mut answer;
        let board_of = target.clone();
        self.edit_doc(target.addr, move |doc| {
            let board = board_of.board(doc);
            if !board.exists() {
                return Err("This canvas has not arrived on this computer yet, so nothing was written.".into());
            }
            let snapshot = board.snapshot();
            let mut scene = Scene::of(&snapshot);
            (scene.pictures, scene.cards) = prepared;
            let edited = board.edit(Stamp::new(author.as_ref(), &now), |t| write(t, &mut scene, &snapshot));
            let result = edited.result.map_err(|e| format!("Nothing was written: {}.", said(&e)))?;
            *answer_ref = Some((result, scene.touched));
            Ok(())
        })
        .await?;
        let (result, touched) = answer.expect("the edit ran");
        self.on_canvas(target, touched).await;
        Ok(result)
    }

    /// Say this agent is on the canvas, with `selection` selected and its
    /// pointer in the middle of it.
    async fn on_canvas(&self, target: &Target, selection: Vec<String>) {
        let Ok(node) = self.api.node(target.addr).await else { return };
        let Ok(doc) = doc_of(&node) else { return };
        let snapshot = target.board(&doc).snapshot();
        let area = selection_bounds(&snapshot, &selection).or_else(|| snapshot.visible_extent());
        let presence = BoardPresence {
            pointer: area.map(|r| BoardPoint { x: r.center().x, y: r.center().y }),
            selection,
            viewport: area
                .map(|r| {
                    let r = r.inflate(80.0);
                    BoardRect { x: r.x, y: r.y, w: r.w, h: r.h }
                })
                .unwrap_or_default(),
            markup: target.text_canvas.clone(),
        };
        self.api.say_on_board(target.addr, presence).await;
    }

    /// The canvas description get_node and get_canvas show.
    pub(super) async fn canvas_body(&self, node: &Node, text_canvas: Option<&str>) -> String {
        self.canvas_body_where(node, text_canvas, None).await
    }

    async fn canvas_body_where(&self, node: &Node, text_canvas: Option<&str>, with_field: Option<&str>) -> String {
        let doc = match doc_of(node) {
            Ok(doc) => doc,
            Err(e) => return e,
        };
        let board = match text_canvas {
            Some(id) => Board::in_text(&doc, id),
            None => Board::of_node(&doc),
        };
        if !board.exists() {
            return "(this canvas has not arrived on this computer yet)\n".to_string();
        }
        let snapshot = board.snapshot();
        let cards = self.card_titles(&snapshot).await;
        describe_where(&snapshot, &cards, with_field)
    }
}

fn sorted(set: &HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = set.iter().cloned().collect();
    v.sort();
    v
}

/// The ids a write made, as a list the model reads: "ref -> id" where a ref was given.
fn made(specs: &[ElementSpec], ids: &[String]) -> String {
    specs
        .iter()
        .zip(ids)
        .enumerate()
        .map(|(i, (spec, id))| match spec.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(name) => format!("- {id} {} ({name})\n", Kind::parse(spec.kind.trim())),
            None => format!("- {id} {} (element {})\n", Kind::parse(spec.kind.trim()), i + 1),
        })
        .collect()
}

/// The board a new canvas starts with, as every canvas starts
/// (`BoardTxn::start_canvas`), then `specs` on top. Answers the element ids.
fn build_canvas(
    t: &mut BoardTxn<'_, '_>,
    picture: Option<&(String, u32, u32)>,
    prepared: Prepared,
    specs: &[ElementSpec],
) -> Result<(Vec<String>, Vec<String>), BoardError> {
    let start = picture.map(|(src, px_w, px_h)| StartingPicture { src, px_w: *px_w, px_h: *px_h });
    let made = t.start_canvas(start)?;
    let mut layers = Vec::new();
    let mut touched = Vec::new();
    if let Some((layer, element)) = made.picture {
        touched.push(element);
        layers.push(LayerInfo { id: layer, name: PICTURE_LAYER_NAME.into(), group: false });
    }
    layers.push(LayerInfo { id: made.draw_layer, name: pimble_board::doc::FIRST_LAYER_NAME.into(), group: false });
    let mut scene = Scene { layers, elements: HashMap::new(), refs: HashMap::new(), pictures: prepared.0, cards: prepared.1, touched: Vec::new() };
    let ids = add_specs(t, &mut scene, specs, None)?;
    touched.extend(ids.iter().cloned());
    Ok((ids, touched))
}

// ── The tools ────────────────────────────────────────────────────────────

#[tool_router(router = canvas_router, vis = "pub(super)")]
impl Pimble {
    /// Read a canvas (a whiteboard, or the marks on a picture) as a list: its
    /// page, its layers, and every element with its id, kind, position and
    /// size (a path as its SVG path data, `d`), text, style, frame or group,
    /// lock and author, and what layers and
    /// elements mean (their notes and fields). `with_field` lists only the
    /// elements with a field ("role=door"). Name a canvas node,
    /// or a note plus the id of a canvas in its text (`#canvas=<id>` in its
    /// picture's URL). render_canvas shows it as a picture.
    #[tool(name = "get_canvas")]
    async fn get_canvas(&self, Parameters(p): Parameters<GetCanvasParams>) -> Result<String, String> {
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        self.on_canvas(&target, Vec::new()).await;
        let with_field = p.with_field.as_deref().map(str::trim).filter(|w| !w.is_empty());
        let body = self.canvas_body_where(&target.node, target.text_canvas.as_deref(), with_field).await;
        Ok(format!("{}\nCoordinates are canvas units; x grows right, y grows down.\n{body}", target.what()))
    }

    /// See a canvas: it rendered as a PNG image, all of it, an area, or some
    /// elements (a frame shows what is in it). The answer says which canvas
    /// area the image shows and at what scale, so pixels map back to canvas
    /// coordinates.
    #[tool(name = "render_canvas")]
    async fn render_canvas(&self, Parameters(p): Parameters<RenderCanvasParams>) -> Result<CallToolResult, String> {
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        let doc = doc_of(&target.node)?;
        let board = target.board(&doc);
        if !board.exists() {
            return Err("This canvas has not arrived on this computer yet.".into());
        }
        let plain = board.snapshot();
        let cards = self.card_titles(&plain).await;
        let labels = cards.iter().map(|(link, title)| (link.clone(), CardLabel { title: title.clone(), color: None })).collect();
        let options = ViewOptions { show_all: p.show_all, card_labels: Arc::new(labels), ..ViewOptions::default() };
        let snapshot = BoardSnapshot::build_with(board.read(), options);
        // The pictures it shows, through the server like the app's loader.
        // Pictures and paint layers' pixels alike.
        let mut pictures: HashMap<String, Arc<Picture>> = HashMap::new();
        for src in snapshot.picture_srcs() {
            if let Some(url) = BlobUrl::parse(&src) {
                if let Ok((_, bytes)) = self.api.get_blob(url.store, url.blob).await {
                    if let Ok(picture) = Picture::decode(&bytes) {
                        pictures.insert(src.clone(), Arc::new(picture));
                    }
                }
            }
        }
        let only: HashSet<String> = p.elements.iter().map(|s| s.trim().to_string()).collect();
        for id in &only {
            if snapshot.get(id).is_none() {
                return Err(format!("There is no element \"{id}\" on this canvas (get_canvas lists the ids)."));
            }
        }
        let rect = match (&p.area, only.is_empty()) {
            (Some(a), _) => Some(Rect::new(a.x, a.y, a.w, a.h).normalized()),
            (None, false) => selection_bounds(&snapshot, &only.iter().cloned().collect::<Vec<_>>()).map(|r| r.inflate(8.0)),
            (None, true) => snapshot.render_rect(),
        };
        let Some(rect) = rect.filter(|r| r.w > 0.0 && r.h > 0.0) else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!("{}: nothing on it shows.", target.what()))]));
        };
        let max = p.max_size.unwrap_or(1024).clamp(64, 2048) as f32;
        let scale = (max / rect.w.max(rect.h)).min(4.0);
        let page_fill = snapshot.page.as_ref().map(|page| page.fill.unwrap_or(Color::WHITE));
        let rendered = match (&p.area, only.is_empty()) {
            (None, true) => pimble_board::render_extent(&snapshot, &pictures, scale),
            (Some(_), _) => pimble_board::render_area(&snapshot, &pictures, rect, scale, Some(page_fill.unwrap_or(Color::WHITE)), None, false),
            (None, false) => pimble_board::render_area(&snapshot, &pictures, rect, scale, None, Some(&only), true),
        };
        let Some(rendered) = rendered else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!("{}: nothing shows there.", target.what()))]));
        };
        // Flattened onto white: a model reads a picture on transparency badly.
        let mut image = rendered.image;
        for px in image.data.chunks_exact_mut(4) {
            let a = px[3] as u32;
            for c in &mut px[..3] {
                *c = ((*c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
            }
            px[3] = 255;
        }
        let png = image.encode_png().map_err(|e| format!("The picture could not be made: {e}"))?;
        let (w, h) = (image.width, image.height);
        let s = w as f32 / rendered.rect.w.max(1e-3);
        self.on_canvas(&target, only.into_iter().collect()).await;
        let missing = snapshot.picture_srcs().iter().filter(|src| !pictures.contains_key(*src)).count();
        let mut note = format!(
            "{}: the image ({w}x{h} px) shows the area {} at {} px per canvas unit; pixel (px, py) is canvas ({} + px/{}, {} + py/{}).",
            target.what(),
            rect_text(&rendered.rect),
            n(s),
            n(rendered.rect.x),
            n(s),
            n(rendered.rect.y),
            n(s)
        );
        if missing > 0 {
            note.push_str(&format!(" {missing} picture(s) on it are not on this computer and show as placeholders."));
        }
        Ok(CallToolResult::success(vec![
            ContentBlock::text(note),
            ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"),
        ]))
    }

    /// Make a canvas node (a whiteboard) under `parent`: empty, or from a
    /// picture file (the picture locked in its own layer, a layer above it to
    /// draw in), with its first elements if given (as add_elements takes them).
    #[tool(name = "create_canvas")]
    async fn create_canvas(&self, Parameters(p): Parameters<CreateCanvasParams>) -> Result<String, String> {
        // A node made under a mount lives in the mount's source store.
        let parent = self.api.create_parent(self.api.resolve(&p.parent).await?).await?;
        // Pictures are stored as writes of the parent, as the app stores a
        // picture dropped on a row, since the canvas does not exist yet.
        let picture = match &p.picture {
            Some(file) => Some(self.store_picture(parent, file).await?),
            None => None,
        };
        let prepared = self.prepare(parent, &p.elements).await?;
        let author = self.board_author().await;
        let now = now();
        let board = Board::standalone();
        let edited = board.edit(Stamp::new(author.as_ref(), &now), |t| build_canvas(t, picture.as_ref(), prepared, &p.elements));
        let ((ids, touched), update) = edited.into_result().map_err(|e| format!("Nothing was made: {}.", said(&e)))?;
        let content = update.ok_or("Nothing was made.")?;
        let title = p
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| p.picture.as_deref().map(picture_title))
            .unwrap_or_else(|| "Canvas".to_string());
        let addr = self.api.create(parent, Place::Last, recipes::canvas(&title, content, picture.is_some())).await?;
        let node = self.api.node(addr).await?;
        self.on_canvas(&Target { addr, node, text_canvas: None }, touched).await;
        let mut out = format!("Made the canvas {}.", self.describe(addr).await);
        if !ids.is_empty() {
            out.push_str(&format!("\nIts elements:\n{}", made(&p.elements, &ids)));
        }
        Ok(out)
    }

    /// Put a canvas into a note's text: a picture file to mark up (the picture
    /// locked in its own layer, a layer above it to draw in), a picture
    /// already in Pimble, or an empty canvas to draw on; with marks to draw on
    /// it at once if given. Answers the canvas's id, which get_canvas and the
    /// other canvas tools take as `canvas`.
    #[tool(name = "insert_canvas")]
    async fn insert_canvas(&self, Parameters(p): Parameters<InsertCanvasParams>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        let node = self.api.node(addr).await?;
        if node.node_type == node_types::BOARD || node.node_type == node_types::SHEET || node.node_type == node_types::DISCUSSION || node.node_type == node_types::MOUNT {
            return Err(format!(
                "{} is a {} and has no text to put a canvas in; add_elements draws on a canvas node.",
                label(&node),
                type_name(&node)
            ));
        }
        let picture = match (&p.picture, &p.src) {
            (Some(file), None) => Some(self.store_picture(addr, file).await?),
            (None, Some(src)) => Some(self.known_picture(src).await?),
            (None, None) => None,
            _ => return Err("Give a picture file or a pimble-blob: src, not both.".into()),
        };
        let prepared = self.prepare(addr, &p.elements).await?;
        let author = self.board_author().await;
        let now = now();
        let canvas = pimble_board::doc::new_id();
        let alt = p.alt.clone().unwrap_or_default().replace(['[', ']', '\n'], " ");
        let src = match &picture {
            Some((src, _, _)) => src.clone(),
            None => pimble_crdt::markdown::IN_TEXT_SRC.to_string(),
        };
        let markdown = format!("![{alt}]({src}{}{canvas})", pimble_crdt::markdown::CANVAS_FRAGMENT);
        let mut made_ids = (Vec::new(), Vec::new());
        let made_ref = &mut made_ids;
        let specs = p.elements.clone();
        let (after, after_section) = (p.after.clone(), p.after_section);
        let canvas_for_edit = canvas.clone();
        // One edit, canvas first then the picture naming it: in the app these
        // are two (the picture lives in the editor), here both are this
        // document's, so a peer never sees one without the other.
        self.edit_doc(addr, move |doc| {
            let board = Board::in_text(doc, &canvas_for_edit);
            let edited = board.edit(Stamp::new(author.as_ref(), &now), |t| build_canvas(t, picture.as_ref(), prepared, &specs));
            *made_ref = edited.result.map_err(|e| format!("Nothing was written: {}.", said(&e)))?;
            match after.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
                Some(quote) => doc.insert_markdown_after(quote, after_section, &markdown),
                None => doc.append_markdown(&markdown),
            }
            .map_err(super::crdt)?;
            Ok(())
        })
        .await?;
        let (ids, touched) = made_ids;
        let fresh = self.api.node(addr).await?;
        self.on_canvas(&Target { addr, node: fresh, text_canvas: Some(canvas.clone()) }, touched).await;
        let mut out = format!(
            "Put canvas {canvas} into {} ({}). Name it to the canvas tools with node {} and canvas \"{canvas}\".",
            label(&node),
            addr.link(),
            addr.link()
        );
        if !ids.is_empty() {
            out.push_str(&format!("\nIts elements:\n{}", made(&p.elements, &ids)));
        }
        Ok(out)
    }

    /// Draw on a canvas: sticky notes, text, shapes (rectangles with a corner
    /// radius, ellipses, diamonds, polygons, stars), vector paths written as SVG
    /// path data (`d`, with fill, stroke, width, cap, join, dash and `rule`),
    /// frames, lines, arrows, connectors between elements (they follow the
    /// elements when moved), pen strokes, numbered steps, callouts, node cards,
    /// pictures and groups, in one edit. Give elements a `ref` to connect or group them in the same call.
    /// Answers each new element's id.
    #[tool(name = "add_elements")]
    async fn add_elements(&self, Parameters(p): Parameters<AddElementsParams>) -> Result<String, String> {
        if p.elements.is_empty() {
            return Err("Give at least one element.".into());
        }
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        let prepared = self.prepare(target.addr, &p.elements).await?;
        let specs = p.elements.clone();
        let layer = p.layer.clone();
        let ids = self
            .canvas_edit(&target, prepared, move |t, scene, _| add_specs(t, scene, &specs, layer.as_deref()))
            .await?;
        Ok(format!("Added to {}:\n{}", target.what(), made(&p.elements, &ids)))
    }

    /// Change elements of a canvas: position, size, points, a path's outline
    /// (`d`, which also turns a shape, line or pen stroke into a path), text,
    /// style, rotation, layer, frame or group, stacking order, lock. Only what a change
    /// names is written, so someone else editing another property keeps theirs.
    #[tool(name = "update_elements")]
    async fn update_elements(&self, Parameters(p): Parameters<UpdateElementsParams>) -> Result<String, String> {
        if p.changes.is_empty() {
            return Err("Give at least one change.".into());
        }
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        let mut cards = HashMap::new();
        for (i, change) in p.changes.iter().enumerate() {
            if let Some(node) = &change.node {
                cards.insert(i, self.card_target(node).await?);
            }
        }
        let changes = p.changes.clone();
        let count = changes.len();
        self.canvas_edit(&target, (HashMap::new(), cards), move |t, scene, snapshot| {
            for (i, change) in changes.iter().enumerate() {
                change_element(t, scene, snapshot, change, i)?;
            }
            Ok(())
        })
        .await?;
        Ok(format!("Changed {count} element{} of {}.", if count == 1 { "" } else { "s" }, target.what()))
    }

    /// Delete elements from a canvas. A frame's or group's members stay (they
    /// no longer belong to it); a connector to a deleted element keeps its last point.
    /// Locked elements, and elements on a locked layer, are kept; the answer
    /// says how many.
    #[tool(name = "delete_elements")]
    async fn delete_elements(&self, Parameters(p): Parameters<DeleteElementsParams>) -> Result<String, String> {
        if p.ids.is_empty() {
            return Err("Name at least one element.".into());
        }
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        let ids: Vec<String> = p.ids.iter().map(|s| s.trim().to_string()).collect();
        let done = self
            .canvas_edit(&target, Default::default(), move |t, scene, _| {
                for id in &ids {
                    if !scene.elements.contains_key(id) {
                        return Err(bad(format!("there is no element \"{id}\" on this canvas")));
                    }
                }
                let done = t.delete_elements(&ids)?;
                if done.deleted.is_empty() {
                    return Err(bad(
                        "every element named is locked, or on a locked layer; update_elements with \"locked\": false, \
                         or update_layer for a locked layer, unlocks them",
                    ));
                }
                Ok(done)
            })
            .await?;
        let (count, kept) = (done.deleted.len(), done.kept.len());
        let mut out = format!("Deleted {count} element{} from {}.", if count == 1 { "" } else { "s" }, target.what());
        if kept > 0 {
            out.push_str(&format!(
                " Kept {kept} locked element{} ({}): update_elements with \"locked\": false, or update_layer for a locked layer, unlocks {}.",
                if kept == 1 { "" } else { "s" },
                done.kept.join(", "),
                if kept == 1 { "it" } else { "them" }
            ));
        }
        Ok(out)
    }

    /// Combine two or more elements into one path: "union", "subtract" (the
    /// bottom-most minus everything above it), "intersect" or "exclude".
    /// Shapes, paths, lines without arrow heads and pen strokes combine,
    /// through their rotation and scale. The result takes the place, style,
    /// note and fields of the top-most one (of the bottom-most for
    /// subtract) and keeps its id; the others are deleted. One edit.
    #[tool(name = "combine_elements")]
    async fn combine_elements(&self, Parameters(p): Parameters<CombineElementsParams>) -> Result<String, String> {
        let op = CombineOp::parse(p.op.trim()).ok_or_else(|| format!("op \"{}\" is not union, subtract, intersect or exclude.", p.op.trim()))?;
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        let ids: Vec<String> = p.ids.iter().map(|s| s.trim().to_string()).collect();
        let count = ids.len();
        let kept = self
            .canvas_edit(&target, Default::default(), move |t, scene, _| {
                for id in &ids {
                    if !scene.elements.contains_key(id) {
                        return Err(bad(format!("there is no element \"{id}\" on this canvas (get_canvas lists the ids)")));
                    }
                }
                let kept = t.combine(&ids, op)?;
                scene.touched.push(kept.clone());
                Ok(kept)
            })
            .await?;
        Ok(format!("Combined {count} elements of {} ({}) into path {kept}; the others are gone.", target.what(), op.as_str()))
    }

    /// Flip elements horizontally or vertically, together, about the center
    /// of all of them. Shapes, paths, lines, strokes and pictures are
    /// mirrored; text, sticky notes, cards, steps, callouts and frames move
    /// to their mirrored place unmirrored (their words stay readable); a
    /// group flips what is in it.
    #[tool(name = "flip_elements")]
    async fn flip_elements(&self, Parameters(p): Parameters<FlipElementsParams>) -> Result<String, String> {
        let axis = match p.direction.trim() {
            "horizontal" => FlipAxis::Horizontal,
            "vertical" => FlipAxis::Vertical,
            other => return Err(format!("direction \"{other}\" is not horizontal or vertical.")),
        };
        if p.ids.is_empty() {
            return Err("Name at least one element.".into());
        }
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        let ids: Vec<String> = p.ids.iter().map(|s| s.trim().to_string()).collect();
        let count = ids.len();
        self.canvas_edit(&target, Default::default(), move |t, scene, _| {
            for id in &ids {
                if !scene.elements.contains_key(id) {
                    return Err(bad(format!("there is no element \"{id}\" on this canvas (get_canvas lists the ids)")));
                }
            }
            t.flip(&ids, axis)?;
            scene.touched.extend(ids.iter().cloned());
            Ok(())
        })
        .await?;
        Ok(format!("Flipped {count} element{} of {} {}.", if count == 1 { "" } else { "s" }, target.what(), p.direction.trim()))
    }

    /// Draw an SVG on a canvas: markup (`svg`) or an .svg file (`file`)
    /// becomes paths with their fills, strokes and gradients, groups, and
    /// simple text, in one edit, placed at x, y (or centered there) and
    /// optionally scaled to `size`. What could not come in (filters, masks,
    /// clip paths, pictures, patterns) is listed in the answer.
    #[tool(name = "import_svg")]
    async fn import_svg(&self, Parameters(p): Parameters<ImportSvgParams>) -> Result<String, String> {
        let markup = match (&p.svg, &p.file) {
            (Some(svg), None) => svg.clone(),
            (None, Some(file)) => {
                let path = std::path::Path::new(file.trim());
                std::fs::read_to_string(path).map_err(|e| format!("The SVG file {} could not be read: {e}", path.display()))?
            }
            _ => return Err("Give the SVG as markup (`svg`) or a file (`file`), one of them.".into()),
        };
        let place = SvgPlacement { at: Point::new(p.x.unwrap_or(0.0), p.y.unwrap_or(0.0)), center: p.center, fit: p.size.filter(|s| *s > 0.0) };
        let import = pimble_board::svg_in::parse(&markup, &place).map_err(|e| format!("Nothing was written: {e}"))?;
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        let layer = p.layer.clone();
        let to_add = import.clone();
        let ids = self
            .canvas_edit(&target, Default::default(), move |t, scene, _| {
                let layer = scene.named_layer(layer.as_deref(), None)?;
                let ids = t.import_svg(&to_add, layer.as_deref())?;
                scene.touched.extend(ids.iter().cloned());
                Ok(ids)
            })
            .await?;
        let mut out = format!("Added to {}:\n", target.what());
        for (item, id) in import.items.iter().zip(&ids) {
            let kind = &item.element.kind;
            match item.group.and_then(|g| ids.get(g)) {
                Some(g) => out.push_str(&format!("- {id} {kind} (in group {g})\n")),
                None => out.push_str(&format!("- {id} {kind}\n")),
            }
        }
        if let Some(b) = import.bounds {
            out.push_str(&format!("Within {}.\n", rect_text(&b)));
        }
        for w in &import.warnings {
            out.push_str(&format!("Left out: {w}\n"));
        }
        Ok(out)
    }

    /// Paint brush strokes on a canvas, as a person paints with the brush:
    /// each stroke is a brush (round, pencil, ink, airbrush, marker,
    /// charcoal, watercolor, highlighter), a color, a size and its points
    /// (optionally with pressure), into a paint layer (the one named, or the
    /// topmost shown and unlocked one, or a new "Paint 1"). `erase` makes an
    /// eraser stroke. With `highlight_picture` the strokes go into the
    /// multiply "Highlights" layer above that picture. render_canvas shows
    /// the result; get_canvas summarizes paint layers. One edit: a person
    /// undoes it with their own undo only when it is theirs, so nothing here
    /// is undone for them.
    #[tool(name = "paint")]
    async fn paint(&self, Parameters(p): Parameters<PaintParams>) -> Result<String, String> {
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        if p.strokes.is_empty() {
            return Err("Give at least one stroke to paint.".into());
        }
        let mut strokes = Vec::with_capacity(p.strokes.len());
        for (i, spec) in p.strokes.iter().enumerate() {
            strokes.push(stroke_of(spec, i).map_err(|e| format!("Nothing was written: {}.", said(&e)))?);
        }
        let (layer, picture) = (p.layer.clone(), p.highlight_picture.clone());
        if layer.is_some() && picture.is_some() {
            return Err("Give `layer` or `highlight_picture`, not both.".into());
        }
        let (layer_id, ids) = self
            .canvas_edit(&target, Default::default(), move |t, scene, snapshot| {
                let layer_id = match (layer.as_deref().map(str::trim).filter(|l| !l.is_empty()), picture.as_deref().map(str::trim)) {
                    (Some(l), _) => scene.layer(l)?.id.clone(),
                    (None, Some(pic)) => {
                        let id = scene.id(pic)?;
                        if !snapshot.get(&id).is_some_and(|e| e.raw.kind == Kind::Picture) {
                            return Err(bad(format!("{pic} is not a picture on this canvas")));
                        }
                        t.highlights_layer(Some(&id))?
                    }
                    (None, None) => t.paint_layer_for(None)?,
                };
                let mut ids = Vec::with_capacity(strokes.len());
                for stroke in strokes {
                    let mut stroke = stroke;
                    stroke.layer = layer_id.clone();
                    ids.push(t.add_stroke(stroke)?);
                }
                Ok((layer_id, ids))
            })
            .await?;
        let doc = doc_of(&self.api.node(target.addr).await?)?;
        let name = target.board(&doc).snapshot().layer(&layer_id).map(|l| l.name.clone()).unwrap_or_default();
        Ok(format!(
            "Painted {} stroke{} in the paint layer \"{name}\" ({layer_id}) of {}: {}.",
            ids.len(),
            if ids.len() == 1 { "" } else { "s" },
            target.what(),
            ids.join(", ")
        ))
    }

    /// Add a layer or a group of layers to a canvas (layers organize; who made
    /// what is on each element).
    #[tool(name = "add_layer")]
    async fn add_layer(&self, Parameters(p): Parameters<AddLayerParams>) -> Result<String, String> {
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        let name = p.name.trim().to_string();
        if name.is_empty() {
            return Err("A layer needs a name.".into());
        }
        let order = p.order.clone().unwrap_or_default();
        let (note, fields) = (p.note.clone(), p.fields.clone());
        let (kind, parent, blend) = (p.kind.clone(), p.parent.clone(), p.blend.clone());
        let id = self
            .canvas_edit(&target, Default::default(), move |t, scene, _| {
                let at = placement(&order, &|l| scene.layer(l).map(|l| l.id.clone()))?;
                let kind = layer_kind(kind.as_deref())?;
                let blend = blend.as_deref().map(blend_mode).transpose()?;
                let parent = match parent.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
                    Some(p) => Some(scene.layer(p)?.id.clone()),
                    None => None,
                };
                let id = t.add_layer_in(&name, kind, parent.as_deref(), at)?;
                if let Some(blend) = blend {
                    t.set_layer_blend(&id, blend)?;
                }
                if let Some(note) = note.as_deref().filter(|n| !n.trim().is_empty()) {
                    t.set_layer_note(&id, note)?;
                }
                for (field, value) in fields.iter().flatten() {
                    t.set_layer_field(&id, field, Some(value))?;
                }
                Ok(id)
            })
            .await?;
        let what = if p.kind.as_deref().map(str::trim) == Some("group") { "group" } else { "layer" };
        Ok(format!("Added {what} \"{}\" ({id}) to {}.", p.name.trim(), target.what()))
    }

    /// Rename, show or hide, lock or unlock, fade, blend, reorder or move into
    /// or out of a group a layer of a canvas, or say what it holds (its note
    /// and fields). Hiding or locking a group hides or locks what is in it.
    /// What a layer shows is the same for everyone who opens the canvas.
    #[tool(name = "update_layer")]
    async fn update_layer(&self, Parameters(p): Parameters<UpdateLayerParams>) -> Result<String, String> {
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        let (layer, name, visible, locked, opacity, order) = (p.layer.clone(), p.name.clone(), p.visible, p.locked, p.opacity, p.order.clone());
        let (note, fields, blend, parent) = (p.note.clone(), p.fields.clone(), p.blend.clone(), p.parent.clone());
        self.canvas_edit(&target, Default::default(), move |t, scene, _| {
            let id = scene.layer(&layer)?.id.clone();
            if let Some(name) = name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                t.rename_layer(&id, name)?;
            }
            if let Some(v) = visible {
                t.set_layer_visible(&id, v)?;
            }
            if let Some(l) = locked {
                t.set_layer_locked(&id, l)?;
            }
            if let Some(o) = opacity {
                t.set_layer_opacity(&id, o)?;
            }
            if let Some(blend) = blend.as_deref() {
                t.set_layer_blend(&id, blend_mode(blend)?)?;
            }
            match (parent.as_deref().map(str::trim), order.as_deref()) {
                (Some(parent), order) => {
                    let at = placement(order.unwrap_or("top"), &|l| scene.layer(l).map(|l| l.id.clone()))?;
                    let group = if parent.is_empty() { None } else { Some(scene.layer(parent)?.id.clone()) };
                    t.move_layer_into(&id, group.as_deref(), at)?;
                }
                (None, Some(order)) => {
                    let at = placement(order, &|l| scene.layer(l).map(|l| l.id.clone()))?;
                    t.move_layer(&id, at)?;
                }
                (None, None) => {}
            }
            if let Some(note) = &note {
                t.set_layer_note(&id, note)?;
            }
            for (field, value) in fields.iter().flatten() {
                t.set_layer_field(&id, field, value.as_deref())?;
            }
            Ok(())
        })
        .await?;
        Ok(format!("Changed layer \"{}\" of {}.", p.layer.trim(), target.what()))
    }

    /// Delete a layer of a canvas, with its elements (a group with every layer
    /// in it), or merge it into the layer below (a group into one layer). A
    /// canvas keeps at least one layer that is not a group.
    #[tool(name = "delete_layer")]
    async fn delete_layer(&self, Parameters(p): Parameters<DeleteLayerParams>) -> Result<String, String> {
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        let (layer, merge) = (p.layer.clone(), p.merge_down);
        self.canvas_edit(&target, Default::default(), move |t, scene, _| {
            let id = scene.layer(&layer)?.id.clone();
            if merge && scene.is_group(&id) {
                t.merge_group(&id)
            } else if merge {
                t.merge_layer_down(&id)
            } else {
                t.delete_layer(&id)
            }
        })
        .await?;
        Ok(format!(
            "{} layer \"{}\" of {}.",
            if p.merge_down { "Merged down" } else { "Deleted" },
            p.layer.trim(),
            target.what()
        ))
    }

    /// A canvas's own settings: a page (the rectangle it prints and renders
    /// as; outside it stays), or none (infinite); and whether its markup shows
    /// (hiding it shows only the pictures, for everyone).
    #[tool(name = "set_canvas")]
    async fn set_canvas(&self, Parameters(p): Parameters<SetCanvasParams>) -> Result<String, String> {
        let wants = [p.page.is_some(), p.page_fits_content, p.remove_page].iter().filter(|b| **b).count();
        if wants > 1 {
            return Err("Give one of page, page_fits_content and remove_page.".into());
        }
        if wants == 0 && p.markup_visible.is_none() {
            return Err("Nothing to set: give a page, page_fits_content, remove_page or markup_visible.".into());
        }
        let target = self.canvas(&p.node, p.canvas.as_deref()).await?;
        let page = match &p.page {
            Some(spec) => Some(Page {
                rect: Rect::new(spec.x, spec.y, spec.w, spec.h).normalized(),
                fill: spec.fill.as_deref().map(|f| color(f, "fill")).transpose().map_err(|e| e.to_string())?,
            }),
            None => None,
        };
        let (fits, remove, markup) = (p.page_fits_content, p.remove_page, p.markup_visible);
        self.canvas_edit(&target, Default::default(), move |t, _, snapshot| {
            if let Some(page) = &page {
                if !(page.rect.w > 0.0 && page.rect.h > 0.0) {
                    return Err(bad("a page needs a width and a height"));
                }
                t.set_page(Some(page))?;
            }
            if fits {
                let rect = snapshot.visible_extent().ok_or_else(|| bad("nothing on this canvas shows, so there is no content to fit"))?;
                t.set_page(Some(&Page { rect, fill: snapshot.page.as_ref().and_then(|p| p.fill) }))?;
            }
            if remove {
                t.set_page(None)?;
            }
            if let Some(v) = markup {
                t.set_markup_visible(v)?;
            }
            Ok(())
        })
        .await?;
        Ok(format!("Set {}.", target.what()))
    }
}

/// A canvas made from a picture file takes the file's name without its extension.
fn picture_title(file: &str) -> String {
    let name = file.trim().rsplit(['/', '\\']).next().unwrap_or(file);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ if name.is_empty() => "Picture".to_string(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: &str) -> ElementSpec {
        ElementSpec { kind: kind.into(), ..Default::default() }
    }

    fn fresh() -> Board {
        let board = Board::standalone();
        board.create(Stamp::new(None, "2026-10-29T00:00:00.000Z")).unwrap();
        board
    }

    fn add(board: &Board, specs: &[ElementSpec]) -> Result<Vec<String>, BoardError> {
        let snapshot = board.snapshot();
        let mut scene = Scene::of(&snapshot);
        board.edit(Stamp::new(None, "2026-10-29T00:00:00.000Z"), |t| add_specs(t, &mut scene, specs, None)).result
    }

    #[test]
    fn a_diagram_with_connectors_by_ref() {
        let board = fresh();
        let a = ElementSpec { name: Some("a".into()), x: Some(0.0), y: Some(0.0), text: Some("Idea".into()), ..spec("sticky") };
        let b = ElementSpec { name: Some("b".into()), x: Some(400.0), y: Some(0.0), w: Some(120.0), h: Some(80.0), text: Some("Plan".into()), ..spec("rect") };
        // The connector comes first in the list and names elements made after it.
        let c = ElementSpec { from: Some("a".into()), to: Some("b".into()), to_side: Some("left".into()), text: Some("leads to".into()), ..spec("connector") };
        let ids = add(&board, &[c, a, b]).unwrap();
        let snap = board.snapshot();
        assert_eq!(snap.elements.len(), 3);
        let connector = snap.get(&ids[0]).unwrap();
        match &connector.raw.geom {
            Geom::Connector { from: End::Element { id: from, .. }, to: End::Element { id: to, side, last } } => {
                assert_eq!((from, to), (&ids[1], &ids[2]));
                assert_eq!(*side, Some(Side::Left));
                assert_eq!((last.x, last.y), (400.0, 40.0), "its last point is the side's middle");
            }
            other => panic!("{other:?}"),
        }
        let text = describe(&snap, &HashMap::new());
        assert!(text.contains("sticky \"Idea\" at 0,0 size 200x200"), "{text}");
        assert!(text.contains(&format!("connector \"leads to\" from {} to {} (left)", ids[1], ids[2])), "{text}");
    }

    #[test]
    fn refusals_write_nothing_and_say_why() {
        let board = fresh();
        let err = add(&board, &[spec("rect")]).unwrap_err();
        assert!(err.to_string().contains("needs x and y"), "{err}");
        let err = add(&board, &[ElementSpec { from: Some("nowhere".into()), to_point: Some(vec![1.0, 2.0]), ..spec("connector") }]).unwrap_err();
        assert!(err.to_string().contains("no element \"nowhere\""), "{err}");
        let err = add(&board, &[spec("blob")]).unwrap_err();
        assert!(err.to_string().contains("is not a kind"), "{err}");
        let err = add(&board, &[ElementSpec { x: Some(0.0), y: Some(0.0), w: Some(1.0), h: Some(1.0), style: Some(StyleSpec { dash: Some("wavy".into()), ..Default::default() }), ..spec("rect") }]).unwrap_err();
        assert!(err.to_string().contains("solid, dashed, dotted"), "{err}");
    }

    #[test]
    fn locked_layers_and_elements_refuse_edits() {
        let board = fresh();
        let ids = add(&board, &[ElementSpec { x: Some(0.0), y: Some(0.0), w: Some(10.0), h: Some(10.0), locked: Some(true), ..spec("rect") }]).unwrap();
        let snapshot = board.snapshot();
        let mut scene = Scene::of(&snapshot);
        let move_it = ElementChange { id: ids[0].clone(), x: Some(50.0), ..Default::default() };
        let err = board.edit(Stamp::new(None, "t"), |t| change_element(t, &mut scene, &snapshot, &move_it, 0)).result.unwrap_err();
        assert!(matches!(err, BoardError::ElementLocked { layer: None, .. }), "{err:?}");
        assert!(said(&err).contains("is locked; update_elements"), "{}", said(&err));
        let unlock_and_move = ElementChange { locked: Some(false), ..move_it };
        board.edit(Stamp::new(None, "t"), |t| change_element(t, &mut scene, &snapshot, &unlock_and_move, 0)).result.unwrap();
        let snap = board.snapshot();
        let e = snap.get(&ids[0]).unwrap();
        assert!(!e.raw.locked);
        assert_eq!(e.raw.geom, Geom::Rect(Rect::new(50.0, 0.0, 10.0, 10.0)));
        // Every layer locked: nowhere to draw.
        let layer = snap.layers[0].id.clone();
        board.edit(Stamp::new(None, "t"), |t| t.set_layer_locked(&layer, true)).result.unwrap();
        let err = add(&board, &[ElementSpec { x: Some(0.0), y: Some(0.0), ..spec("step") }]).unwrap_err();
        assert_eq!(err, BoardError::EveryLayerLocked);
        assert!(said(&err).contains("every layer of this canvas is locked"), "{}", said(&err));
        // An element on the locked layer takes no lock of its own either.
        let snapshot = board.snapshot();
        let mut scene = Scene::of(&snapshot);
        let lock = ElementChange { id: ids[0].clone(), locked: Some(true), ..Default::default() };
        let err = board.edit(Stamp::new(None, "t"), |t| change_element(t, &mut scene, &snapshot, &lock, 0)).result.unwrap_err();
        assert!(said(&err).contains("on the locked layer \"Layer 1\""), "{}", said(&err));
    }

    #[test]
    fn a_member_that_is_locked_stays_out_of_a_group() {
        let board = fresh();
        let ids = add(&board, &[ElementSpec { name: Some("a".into()), x: Some(0.0), y: Some(0.0), w: Some(10.0), h: Some(10.0), locked: Some(true), ..spec("rect") }])
            .unwrap();
        let err = add(&board, &[ElementSpec { members: Some(vec![ids[0].clone()]), ..spec("group") }]).unwrap_err();
        assert!(matches!(&err, BoardError::ElementLocked { id, .. } if *id == ids[0]), "{err:?}");
    }

    #[test]
    fn groups_take_members_and_strokes_take_points() {
        let board = fresh();
        let ids = add(
            &board,
            &[
                ElementSpec { name: Some("s".into()), points: Some(vec![vec![0.0, 0.0], vec![10.0, 5.0], vec![20.0, 0.0]]), ..spec("pen") },
                ElementSpec { name: Some("l".into()), points: Some(vec![vec![0.0, 20.0], vec![20.0, 20.0]]), ..spec("arrow") },
                ElementSpec { members: Some(vec!["s".into(), "l".into()]), ..spec("group") },
            ],
        )
        .unwrap();
        let snap = board.snapshot();
        assert_eq!(snap.get(&ids[0]).unwrap().raw.points.len(), 3);
        assert_eq!(snap.get(&ids[0]).unwrap().parent.as_deref(), Some(ids[2].as_str()));
        assert_eq!(snap.get(&ids[1]).unwrap().parent.as_deref(), Some(ids[2].as_str()));
    }

    #[test]
    fn smooth_strokes_pass_through_their_points_and_close() {
        let pts = [Point::new(0.0, 0.0), Point::new(100.0, 0.0), Point::new(100.0, 100.0), Point::new(0.0, 100.0)];
        let open = stroke_points(&pts, true, false);
        assert_eq!(open.len(), 3 * 8 + 1);
        for p in &pts {
            assert!(open.iter().any(|q| (q[0] - p.x).abs() < 1e-3 && (q[1] - p.y).abs() < 1e-3), "passes through {p:?}");
        }
        let closed = stroke_points(&pts, true, true);
        assert_eq!(closed.len(), 4 * 8 + 1);
        assert_eq!((closed[0][0], closed[0][1]), (closed[32][0], closed[32][1]), "ends where it began");
        assert_eq!(stroke_points(&pts, false, true).len(), 5, "straight, closed");
    }

    #[test]
    fn a_centered_label_sits_on_its_point() {
        let board = fresh();
        let ids = add(&board, &[ElementSpec { x: Some(100.0), y: Some(50.0), center: Some(true), text: Some("Kitchen".into()), ..spec("text") }]).unwrap();
        let snap = board.snapshot();
        let tb = snap.get(&ids[0]).unwrap().text_box.unwrap();
        assert!((tb.center().x - 100.0).abs() < 0.5 && (tb.center().y - 50.0).abs() < 0.5, "{tb:?}");
    }

    #[test]
    fn meaning_goes_in_reads_back_and_filters() {
        let board = fresh();
        let mut fields = BTreeMap::new();
        fields.insert("role".to_string(), "door".to_string());
        fields.insert("swing".to_string(), "in".to_string());
        let ids = add(
            &board,
            &[
                ElementSpec { x: Some(0.0), y: Some(0.0), w: Some(90.0), h: Some(10.0), note: Some("Front door".into()), fields: Some(fields), locked: Some(true), ..spec("rect") },
                ElementSpec { x: Some(200.0), y: Some(0.0), w: Some(10.0), h: Some(10.0), ..spec("rect") },
            ],
        )
        .unwrap();
        let snap = board.snapshot();
        let text = describe_where(&snap, &HashMap::new(), Some("ROLE=Door"));
        assert!(text.contains(&ids[0]) && !text.contains(&ids[1]), "{text}");
        assert!(text.contains("note: Front door") && text.contains("fields: role = door; swing = in"), "{text}");
        // A locked element can still be described, and a field removed.
        let mut scene = Scene::of(&snap);
        let mut change_fields = BTreeMap::new();
        change_fields.insert("Swing".to_string(), None);
        change_fields.insert("width".to_string(), Some("90 cm".to_string()));
        let change = ElementChange { id: ids[0].clone(), note: Some("Front door, oak".into()), fields: Some(change_fields), ..Default::default() };
        board.edit(Stamp::new(None, "t"), |t| change_element(t, &mut scene, &snap, &change, 0)).result.unwrap();
        let e = board.snapshot();
        let e = e.get(&ids[0]).unwrap();
        assert_eq!(e.raw.note.as_deref(), Some("Front door, oak"));
        assert_eq!(e.raw.fields, vec![("role".to_string(), "door".to_string()), ("width".to_string(), "90 cm".to_string())]);
        assert!(e.raw.locked, "still locked");
    }

    #[test]
    fn canvas_ids_come_from_urls_too() {
        assert_eq!(canvas_id("pimble-blob:s/h#canvas=abc"), "abc");
        assert_eq!(canvas_id(" abc "), "abc");
        assert_eq!(canvas_id("pimble-blob:s/h#board=old"), "old");
        assert_eq!(picture_title("/tmp/Screenshot 1.png"), "Screenshot 1");
    }

    #[test]
    fn get_canvas_shows_groups_kinds_and_blends() {
        let board = fresh();
        let st = Stamp::new(None, "2026-10-29T00:00:00.000Z");
        board
            .edit(st, |t| {
                let g = t.add_group("Shading", None, Placement::Top)?;
                let a = t.add_layer_in("Shadows", LayerKind::Vector, Some(&g), Placement::Top)?;
                t.add_layer_in("Wash", LayerKind::Paint, Some(&g), Placement::Top)?;
                t.set_layer_blend(&a, blend_mode("multiply")?)?;
                t.set_layer_locked(&g, true)
            })
            .into_result()
            .unwrap();
        let text = describe(&board.snapshot(), &HashMap::new());
        assert!(text.contains("## Group \"Shading\""), "{text}");
        assert!(text.contains("locked, 2 layers"), "{text}");
        assert!(text.contains("### Layer \"Shadows\""), "{text}");
        assert!(text.contains("in group \"Shading\", blend multiply, locked by its group"), "{text}");
        assert!(text.contains("### Paint layer \"Wash\""), "{text}");
        assert!(text.find("Group \"Shading\"").unwrap() < text.find("Layer \"Shadows\"").unwrap(), "a group before what it holds");
        assert!(layer_kind(Some("sketch")).is_err());
        assert!(blend_mode("burn").is_err());
    }

    #[test]
    fn strokes_go_into_a_paint_layer_and_get_canvas_summarizes_it() {
        let board = fresh();
        let agent = Author { id: "agent".into(), name: "Claude".into() };
        let st = Stamp::new(Some(&agent), "2026-11-17T00:00:00.000Z");
        let spec = |preset: Option<&str>, erase: bool, points: Vec<Vec<f32>>| StrokeSpec {
            preset: preset.map(str::to_string),
            color: Some("#228be6".into()),
            size: Some(12.0),
            opacity: None,
            erase,
            points,
        };
        let a = stroke_of(&spec(Some("watercolor"), false, vec![vec![0.0, 0.0], vec![50.0, 10.0, 0.4], vec![100.0, 0.0]]), 0).unwrap();
        assert_eq!(a.points[1], [50.0, 10.0, 0.4, 0.0]);
        assert_eq!(a.points[0][2], 1.0, "no pressure reads as full");
        assert_eq!(a.settings.size, 12.0);
        let b = stroke_of(&spec(None, true, vec![vec![40.0, 0.0]]), 1).unwrap();
        assert_eq!((b.preset.as_str(), b.blend), ("round", StrokeBlend::Erase));
        assert!(said(&stroke_of(&spec(Some("crayon"), false, vec![vec![0.0, 0.0]]), 2).unwrap_err()).contains("the brushes are round, pencil"));
        assert!(stroke_of(&spec(None, false, vec![vec![0.0]]), 3).is_err());
        assert!(stroke_of(&spec(None, false, vec![]), 3).is_err());
        let layer = board
            .edit(st, |t| {
                let layer = t.paint_layer_for(None)?;
                for mut s in [a, b] {
                    s.layer = layer.clone();
                    t.add_stroke(s)?;
                }
                Ok(layer)
            })
            .into_result()
            .unwrap()
            .0;
        let text = describe(&board.snapshot(), &HashMap::new());
        assert!(text.contains(&format!("## Paint layer \"Paint 1\" ({layer})")), "{text}");
        assert!(text.contains("1 brush stroke, 1 eraser stroke · brushes watercolor x1 · colors #228be6 · paints the area at"), "{text}");
        assert!(text.contains("by Claude"), "{text}");
        let vector = board.snapshot().layers[0].id.clone();
        let refused = board.edit(st, |t| t.add_stroke(NewStroke::new(vector, &pimble_board::brush::ROUND, Color::BLACK).with_points(vec![[0.0, 0.0, 1.0, 0.0]])));
        assert!(said(&refused.result.unwrap_err()).contains("strokes go in a paint layer"));
    }

    #[test]
    fn paths_go_in_as_path_data_and_come_back_as_it() {
        let board = fresh();
        let path = ElementSpec {
            d: Some("M0 0 H100 V100 H0 Z M30 30 h40 v40 h-40 z".into()),
            rule: Some("evenodd".into()),
            style: Some(StyleSpec { fill: Some("#3366ff".into()), cap: Some("square".into()), join: Some("bevel".into()), dash: Some("8 4".into()), ..Default::default() }),
            ..spec("path")
        };
        let star = ElementSpec { x: Some(200.0), y: Some(0.0), w: Some(100.0), h: Some(100.0), style: Some(StyleSpec { points: Some(7), inner: Some(0.5), ..Default::default() }), ..spec("star") };
        let poly = ElementSpec { x: Some(400.0), y: Some(0.0), w: Some(100.0), h: Some(100.0), ..spec("polygon") };
        let rect = ElementSpec { x: Some(600.0), y: Some(0.0), w: Some(100.0), h: Some(60.0), style: Some(StyleSpec { radius: Some(12.0), ..Default::default() }), ..spec("rect") };
        let ids = add(&board, &[path, star, poly, rect]).unwrap();
        let snap = board.snapshot();
        let text = describe(&snap, &HashMap::new());
        assert!(text.contains(&format!("{} path d=\"M0 0L100 0L100 100L0 100ZM30 30L70 30L70 70L30 70Z\", rule evenodd", ids[0])), "{text}");
        assert!(text.contains("dash \"8 4\", cap square, join bevel"), "{text}");
        assert!(text.contains("star at 200,0 size 100x100 · 7 points, inner 0.5"), "{text}");
        assert!(text.contains("polygon at 400,0 size 100x100 · 6 sides"), "{text}");
        assert!(text.contains("radius 12"), "{text}");

        // A new outline for the rectangle turns it into a path; x and w stretch a path.
        let st = Stamp::new(None, "2026-10-29T00:00:00.000Z");
        let change = |c: ElementChange| {
            let snapshot = board.snapshot();
            let mut scene = Scene::of(&snapshot);
            board.edit(st, |t| change_element(t, &mut scene, &snapshot, &c, 0)).result
        };
        change(ElementChange { id: ids[3].clone(), d: Some("M600 0 L700 0 L650 80 Z".into()), ..Default::default() }).unwrap();
        let snap = board.snapshot();
        let e = snap.get(&ids[3]).unwrap();
        assert_eq!(e.raw.kind, Kind::Path);
        assert_eq!(e.raw.style.radius, Some(12.0), "its style stays");
        change(ElementChange { id: ids[3].clone(), x: Some(0.0), w: Some(50.0), rule: Some("evenodd".into()), ..Default::default() }).unwrap();
        let snap = board.snapshot();
        let Geom::Path(p) = &snap.get(&ids[3]).unwrap().raw.geom else { panic!() };
        assert_eq!(p.bounds().unwrap(), Rect::new(0.0, 0.0, 50.0, 80.0));
        assert_eq!(p.rule, PathRule::EvenOdd);
        // A preset dash replaces a custom pattern.
        change(ElementChange { id: ids[0].clone(), style: Some(StyleSpec { dash: Some("dotted".into()), ..Default::default() }), ..Default::default() }).unwrap();
        let snap = board.snapshot();
        let s = &snap.get(&ids[0]).unwrap().raw.style;
        assert_eq!((s.dash, s.dash_pattern.clone()), (Some(Dash::Dotted), None));

        // Refusals say why and write nothing.
        let err = add(&board, &[ElementSpec { d: Some("L 1 2".into()), ..spec("path") }]).unwrap_err();
        assert!(err.to_string().contains("d: the path data has a mistake"), "{err}");
        let err = add(&board, &[spec("path")]).unwrap_err();
        assert!(err.to_string().contains("needs its outline"), "{err}");
        let err = add(&board, &[ElementSpec { d: Some("M0 0 L1 1".into()), rule: Some("odd".into()), ..spec("path") }]).unwrap_err();
        assert!(err.to_string().contains("not nonzero or evenodd"), "{err}");
        let err = add(&board, &[ElementSpec { x: Some(0.0), y: Some(0.0), w: Some(1.0), h: Some(1.0), style: Some(StyleSpec { sides: Some(2), ..Default::default() }), ..spec("polygon") }]).unwrap_err();
        assert!(err.to_string().contains("3 sides or more"), "{err}");
        let arrow = add(&board, &[ElementSpec { points: Some(vec![vec![0.0, 0.0], vec![5.0, 5.0]]), ..spec("arrow") }]).unwrap();
        let err = change(ElementChange { id: arrow[0].clone(), d: Some("M0 0 L1 1".into()), ..Default::default() }).unwrap_err();
        assert!(err.to_string().contains("cannot take path data"), "{err}");
    }

    #[test]
    fn gradients_go_in_read_back_and_give_way_to_a_solid_color() {
        let board = fresh();
        let grad = GradientSpec {
            kind: Some("radial".into()),
            from: None,
            to: None,
            stops: vec![StopSpec { offset: None, color: "#ffffff".into() }, StopSpec { offset: None, color: "#00000080".into() }],
        };
        let rect = ElementSpec {
            x: Some(0.0),
            y: Some(0.0),
            w: Some(100.0),
            h: Some(50.0),
            style: Some(StyleSpec { fill_gradient: Some(grad), ..Default::default() }),
            ..spec("rect")
        };
        let ids = add(&board, &[rect]).unwrap();
        let snap = board.snapshot();
        let text = describe(&snap, &HashMap::new());
        assert!(text.contains("fill radial gradient from 0.5,0.5 to 1,0.5 (#ffffff at 0, #00000080 at 1)"), "{text}");
        let e = snap.get(&ids[0]).unwrap();
        assert_eq!(e.raw.style.fill, Some(Color::WHITE), "the fallback an older app draws");
        // A solid fill given later replaces it.
        let st = Stamp::new(None, "2026-10-29T00:00:00.000Z");
        let snapshot = board.snapshot();
        let mut scene = Scene::of(&snapshot);
        let change = ElementChange { id: ids[0].clone(), style: Some(StyleSpec { fill: Some("#ff0000".into()), ..Default::default() }), ..Default::default() };
        board.edit(st, |t| change_element(t, &mut scene, &snapshot, &change, 0)).result.unwrap();
        let snap = board.snapshot();
        let s = &snap.get(&ids[0]).unwrap().raw.style;
        assert_eq!((s.fill, s.fill_gradient.clone()), (Some(Color::rgb(255, 0, 0)), None));
        // Refusals.
        let one = GradientSpec { stops: vec![StopSpec { offset: Some(0.0), color: "#fff".into() }], ..Default::default() };
        let err = style_of(&StyleSpec { fill_gradient: Some(one), ..Default::default() }).unwrap_err();
        assert!(err.to_string().contains("two stops or more"), "{err}");
        let bad_kind = GradientSpec { kind: Some("conic".into()), stops: vec![StopSpec { offset: None, color: "#fff".into() }; 2], ..Default::default() };
        assert!(style_of(&StyleSpec { fill_gradient: Some(bad_kind), ..Default::default() }).is_err());
    }
}
