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
//! What the app's tools refuse a person these refuse an agent: a locked layer
//! or element takes no edits until it is unlocked.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use base64::Engine;
use pimble_board::{
    selection_bounds, Align, ArrowHeads, Author, Board, BoardError, BoardSnapshot, BoardTxn, CardLabel, Color, Dash, End,
    Geom, Kind, NewElement, Page, Picture, PictureGeom, Placement, Point, Rect, Side, Stamp, Style, StyleKey, Transform,
    ViewOptions,
};
use pimble_client::PimbleClient;
use pimble_core::{custom_keys, node_types, BlobUrl, Node, Place, PimbleUrl};
use pimble_crdt::NodeDoc;
use pimble_rpc::{BoardPoint, BoardPresence, BoardRect};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{rpc, Pimble};
use crate::nodes::{self, label, Addr};

/// Board writes are stamped with RFC 3339 UTC with milliseconds: step numbers
/// sort by it as a string, so every writer uses the app's shape.
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The icons the app gives a new canvas: empty, and made from a picture.
const CANVAS_ICON: &str = "chalkboard";
const PHOTO_ICON: &str = "photo";
/// The sizes the app's tools give a new sticky note and node card.
const STICKY_SIZE: f32 = 200.0;
const CARD_SIZE: (f32, f32) = (240.0, 56.0);
/// How many elements get_canvas lists before it stops.
const LIST_LIMIT: usize = 400;

/// What a list of specs needs from outside the document, by spec index:
/// each picture's URL and pixel size, each card's link and title.
type Prepared = (HashMap<usize, (String, u32, u32)>, HashMap<usize, (String, String)>);

// ── Parameters ───────────────────────────────────────────────────────────

#[derive(Deserialize, JsonSchema)]
pub struct CanvasParam {
    /// A canvas node, or a note whose text holds the canvas: a pimble: link
    /// (or the pimble-board: address of an embedded canvas), a node id, or a path.
    pub node: String,
    /// For a picture's canvas in a note's text: its id, the part after
    /// `#canvas=` in the picture's URL as get_node shows it (the whole URL works
    /// too). Leave it out for a canvas node.
    #[serde(default)]
    pub canvas: Option<String>,
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
    /// "solid", "dashed" or "dotted".
    #[serde(default)]
    pub dash: Option<String>,
}

/// One element to add. Coordinates are canvas units (one unit is one pixel of a
/// picture placed at 100%); x grows right, y grows down.
#[derive(Deserialize, JsonSchema, Clone, Default)]
pub struct ElementSpec {
    /// One of: sticky, text, rect, ellipse, diamond, frame, line, arrow,
    /// connector, pen, highlighter, step, callout, card, picture, spotlight, group.
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
    /// opacity, font_size, align, shadow, arrow_heads, dash.
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
pub struct AddLayerParams {
    pub node: String,
    #[serde(default)]
    pub canvas: Option<String>,
    pub name: String,
    /// Where it goes: "top" (the default), "bottom", "above:<layer>" or
    /// "below:<layer>" (a layer's name or id).
    #[serde(default)]
    pub order: Option<String>,
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
    /// "top", "bottom", "above:<layer>" or "below:<layer>".
    #[serde(default)]
    pub order: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteLayerParams {
    pub node: String,
    #[serde(default)]
    pub canvas: Option<String>,
    /// The layer: its name or id.
    pub layer: String,
    /// Keep its elements: they go on top of the layer below ("Merge Down").
    /// Left out, they are deleted with it.
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
    pick(&spec.dash, "dash", &|s| Dash::parse(s).is_some(), "solid, dashed, dotted")?;
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
        dash: spec.dash.as_deref().and_then(|s| Dash::parse(s.trim())),
    })
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
    locked: bool,
}

/// The canvas as it was when the write began, and the elements this write made.
struct Scene {
    layers: Vec<LayerInfo>,
    /// By id: its bounds, whether it is locked (itself or by its layer), its layer.
    elements: HashMap<String, (Rect, bool, String)>,
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
            .map(|l| LayerInfo { id: l.id.clone(), name: l.name.clone(), locked: l.locked })
            .collect::<Vec<_>>();
        let locked_layers: HashSet<&str> = snapshot.layers.iter().filter(|l| l.locked).map(|l| l.id.as_str()).collect();
        let elements = snapshot
            .elements
            .iter()
            .map(|e| (e.raw.id.clone(), (e.bounds, e.raw.locked || locked_layers.contains(e.layer.as_str()), e.layer.clone())))
            .collect();
        Self { layers, elements, refs: HashMap::new(), pictures: HashMap::new(), cards: HashMap::new(), touched: Vec::new() }
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

    /// The layer a new element goes in.
    fn target_layer(&self, own: Option<&str>, call: Option<&str>) -> Result<String, BoardError> {
        match own.or(call).map(str::trim).filter(|s| !s.is_empty()) {
            Some(name) => {
                let layer = self.layer(name)?;
                if layer.locked {
                    return Err(bad(format!("layer \"{}\" is locked; update_layer can unlock it", layer.name)));
                }
                Ok(layer.id.clone())
            }
            None => self
                .layers
                .iter()
                .rev()
                .find(|l| !l.locked)
                .map(|l| l.id.clone())
                .ok_or_else(|| bad("every layer of this canvas is locked; update_layer can unlock one, or add_layer adds one")),
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
                let last = self.elements.get(&id).map(|(r, _, _)| anchor_of(r, side)).unwrap_or(Point::new(0.0, 0.0));
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
        Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Frame | Kind::Spotlight => Geom::Rect(rect(kind.as_str(), None)?),
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
                "\"{k}\" is not a kind; use sticky, text, rect, ellipse, diamond, frame, line, arrow, connector, pen, \
                 highlighter, step, callout, card, picture, spotlight or group"
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
        let layer = scene.target_layer(spec.layer.as_deref(), call_layer)?;
        let mut element = NewElement::new(kind.clone(), geom.clone()).in_layer(layer.clone()).with_points(points.clone());
        if let Some(text) = &spec.text {
            if matches!(kind, Kind::Pen | Kind::Highlighter | Kind::Picture | Kind::Group | Kind::Step | Kind::Line | Kind::Arrow) {
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
        scene.elements.insert(id.clone(), (rect_of_geom(&geom, &points), false, layer));
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
            if scene.elements.get(&member).is_some_and(|(_, locked, _)| *locked) {
                return Err(bad(format!("element {member} is locked; unlock it before putting it in a group or frame")));
            }
            t.set_parent(&member, Some(id))?;
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

/// Apply one change to an element (refused, before anything is written,
/// when the element or its layer is locked and the change is not unlocking it).
fn change_element(t: &mut BoardTxn<'_, '_>, scene: &mut Scene, snapshot: &BoardSnapshot, change: &ElementChange, index: usize) -> Result<(), BoardError> {
    let id = change.id.trim().to_string();
    let element = snapshot.get(&id).ok_or_else(|| bad(format!("there is no element \"{id}\" on this canvas (get_canvas lists the ids)")))?;
    let layer_locked = snapshot.layer(&element.layer).is_some_and(|l| l.locked);
    if layer_locked {
        let name = snapshot.layer(&element.layer).map(|l| l.name.clone()).unwrap_or_default();
        return Err(bad(format!("element {id} is on the locked layer \"{name}\"; update_layer can unlock it")));
    }
    if element.raw.locked {
        if change.locked != Some(false) {
            return Err(bad(format!("element {id} is locked; set \"locked\": false in the same change to edit it")));
        }
        t.set_locked(&id, false)?;
    }
    let raw = &element.raw;
    // Geometry, merged into what it has.
    let r = |r: &Rect| Rect::new(change.x.unwrap_or(r.x), change.y.unwrap_or(r.y), change.w.unwrap_or(r.w), change.h.unwrap_or(r.h)).normalized();
    let new_points = || -> Result<Option<Vec<Point>>, BoardError> {
        change.points.as_ref().map(|ps| ps.iter().map(|p| point(p, "each point")).collect()).transpose()
    };
    let geom = match &raw.geom {
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
    };
    if let Some(geom) = geom.filter(|g| *g != raw.geom) {
        t.set_geom(&id, &geom)?;
    }
    if let Some(text) = &change.text {
        t.set_text(&id, text)?;
    }
    if let Some(style) = &change.style {
        t.set_style(&id, &style_of(style)?)?;
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
        let layer = scene.target_layer(Some(layer), None)?;
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
    if let Some(c) = style.stroke {
        out.push(format!("stroke {}", c.to_hex()));
    }
    if let Some(c) = style.fill {
        out.push(if c.a == 0 { "no fill".to_string() } else { format!("fill {}", c.to_hex()) });
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
    out
}

/// One line per element, grouped by layer, bottom to top: everything an
/// assistant needs to name, place and change it.
fn describe(snapshot: &BoardSnapshot, cards: &HashMap<String, String>) -> String {
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
        if !layer.visible {
            flags.push("hidden".to_string());
        }
        if layer.locked {
            flags.push("locked".to_string());
        }
        if layer.opacity < 1.0 {
            flags.push(format!("opacity {}", n(layer.opacity)));
        }
        out.push_str(&format!(
            "\n## Layer \"{}\" ({}){}, {} element{}, top first\n",
            layer.name,
            layer.id,
            if flags.is_empty() { String::new() } else { format!(", {}", flags.join(", ")) },
            layer.count,
            if layer.count == 1 { "" } else { "s" }
        ));
        for e in snapshot.elements.iter().rev().filter(|e| e.layer == layer.id) {
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
            let style = style_text(&raw.style);
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
        }
    }
    out
}

// ── The tools' shared steps ──────────────────────────────────────────────

impl Pimble {
    /// Who this agent's elements are stamped with: its id and name, as the
    /// server knows them (none when no account is signed in, as for a person).
    async fn board_author(&self, client: &PimbleClient) -> Option<Author> {
        match client.who_am_i().await {
            Ok((Some(sub), name)) => Some(Author::new(sub, if name.is_empty() { "AI agent".to_string() } else { name })),
            _ => None,
        }
    }

    /// The canvas a tool was given.
    async fn canvas(&self, client: &PimbleClient, node: &str, canvas: Option<&str>) -> Result<Target, String> {
        let node = node.trim();
        // An embedded canvas node's address, as get_node shows it in text.
        let node = match node.strip_prefix("pimble-board:") {
            Some(rest) if rest.contains('/') => format!("pimble:{}", rest.split('#').next().unwrap_or(rest)),
            _ => node.to_string(),
        };
        let addr = nodes::resolve(client, &node).await?;
        let found = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
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
    async fn card_titles(&self, client: &PimbleClient, snapshot: &BoardSnapshot) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for e in &snapshot.elements {
            if let Geom::Card { link, .. } = &e.raw.geom {
                if out.contains_key(link) {
                    continue;
                }
                if let Some(url) = PimbleUrl::parse(link) {
                    if let Ok(node) = client.get_node(url.store, url.node).await {
                        out.insert(link.clone(), label(&node));
                    }
                }
            }
        }
        out
    }

    /// Store a picture file as a blob of `addr`'s store (as a write of
    /// `addr`), fitted as the app fits one: its URL and pixel size.
    async fn store_picture(&self, client: &PimbleClient, addr: Addr, file: &str) -> Result<(String, u32, u32), String> {
        let path = std::path::Path::new(file.trim());
        let bytes = std::fs::read(path).map_err(|e| format!("The picture {} could not be read: {e}", path.display()))?;
        let fitted = pimble_image::fit(bytes).map_err(|e| e.to_string())?;
        let picture = Picture::decode(&fitted.bytes).map_err(|e| format!("{} is not a picture Pimble can show: {e}", path.display()))?;
        let url = client.put_blob(addr.store, addr.node, fitted.mime.as_str(), &fitted.bytes).await.map_err(rpc)?;
        Ok((url.to_string(), picture.width(), picture.height()))
    }

    /// A picture already in Pimble: its URL and pixel size.
    async fn known_picture(&self, client: &PimbleClient, src: &str) -> Result<(String, u32, u32), String> {
        let src = src.trim().split('#').next().unwrap_or_default();
        let url = BlobUrl::parse(src).ok_or_else(|| format!("\"{src}\" is not a pimble-blob: URL."))?;
        let (_, bytes) = client.get_blob(url.store, url.blob).await.map_err(rpc)?;
        let picture = Picture::decode(&bytes).map_err(|e| format!("{src} is not a picture Pimble can show: {e}"))?;
        Ok((url.to_string(), picture.width(), picture.height()))
    }

    /// Everything a list of specs needs from outside the document: pictures
    /// stored, cards' nodes found. Picture blobs are written as writes of `owner`.
    async fn prepare(
        &self,
        client: &PimbleClient,
        owner: Addr,
        specs: &[ElementSpec],
    ) -> Result<Prepared, String> {
        let mut pictures = HashMap::new();
        let mut cards = HashMap::new();
        for (i, spec) in specs.iter().enumerate() {
            let kind = Kind::parse(spec.kind.trim());
            if kind == Kind::Picture {
                let picture = match (&spec.file, &spec.src) {
                    (Some(file), None) => self.store_picture(client, owner, file).await?,
                    (None, Some(src)) => self.known_picture(client, src).await?,
                    _ => return Err(format!("Element {} (a picture) takes a file or a src, one of them.", i + 1)),
                };
                pictures.insert(i, picture);
            }
            if let Some(node) = &spec.node {
                cards.insert(i, self.card_target(client, node).await?);
            } else if kind == Kind::Card {
                return Err(format!("Element {} (a card) needs the node it shows (`node`).", i + 1));
            }
        }
        Ok((pictures, cards))
    }

    async fn card_target(&self, client: &PimbleClient, node: &str) -> Result<(String, String), String> {
        let addr = nodes::resolve(client, node).await?;
        let found = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        Ok((addr.link(), label(&found)))
    }

    /// One write to a canvas: `write` runs in one board transaction over the
    /// canvas as it is in the document fetched for the write, and what it
    /// changed goes through `applyEdit`. Then the agent is on the canvas with
    /// what it touched selected. Answers what `write` answered.
    async fn canvas_edit<R: Send + 'static>(
        &self,
        client: &PimbleClient,
        target: &Target,
        prepared: Prepared,
        write: impl FnOnce(&mut BoardTxn<'_, '_>, &mut Scene, &BoardSnapshot) -> Result<R, BoardError> + Send,
    ) -> Result<R, String> {
        let author = self.board_author(client).await;
        let now = now();
        let mut answer: Option<(R, Vec<String>)> = None;
        let answer_ref = &mut answer;
        let board_of = target.clone();
        self.edit_doc(client, target.addr, move |doc| {
            let board = board_of.board(doc);
            if !board.exists() {
                return Err("This canvas has not arrived on this computer yet, so nothing was written.".into());
            }
            let snapshot = board.snapshot();
            let mut scene = Scene::of(&snapshot);
            (scene.pictures, scene.cards) = prepared;
            let edited = board.edit(Stamp::new(author.as_ref(), &now), |t| write(t, &mut scene, &snapshot));
            let result = edited.result.map_err(|e| format!("Nothing was written: {e}."))?;
            *answer_ref = Some((result, scene.touched));
            Ok(())
        })
        .await?;
        let (result, touched) = answer.expect("the edit ran");
        self.on_canvas(client, target, touched).await;
        Ok(result)
    }

    /// Say this agent is on the canvas, with `selection` selected and its
    /// pointer in the middle of it.
    async fn on_canvas(&self, client: &PimbleClient, target: &Target, selection: Vec<String>) {
        let Ok(node) = client.get_node(target.addr.store, target.addr.node).await else { return };
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
        self.presence.say_on_board(client, target.addr, presence).await;
    }

    /// The canvas description get_node and get_canvas show.
    pub(super) async fn canvas_body(&self, client: &PimbleClient, node: &Node, text_canvas: Option<&str>) -> String {
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
        let cards = self.card_titles(client, &snapshot).await;
        describe(&snapshot, &cards)
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

/// The board a new canvas starts with, as the app makes one: "Layer 1" and,
/// for a picture, the picture at 100% locked in a layer "Picture" below it.
/// Then `specs` on top. Answers the element ids.
fn build_canvas(
    t: &mut BoardTxn<'_, '_>,
    picture: Option<&(String, u32, u32)>,
    prepared: Prepared,
    specs: &[ElementSpec],
) -> Result<(Vec<String>, Vec<String>), BoardError> {
    let first = t.create()?;
    let mut layers = Vec::new();
    let mut touched = Vec::new();
    if let Some((src, w, h)) = picture {
        let layer = t.add_layer("Picture", Placement::Bottom)?;
        let geom = PictureGeom { rect: Rect::new(0.0, 0.0, *w as f32, *h as f32), src: src.clone(), px_w: *w, px_h: *h, crop: None };
        touched.push(t.add_element(NewElement::new(Kind::Picture, Geom::Picture(geom)).in_layer(layer.clone()))?);
        t.set_layer_locked(&layer, true)?;
        layers.push(LayerInfo { id: layer, name: "Picture".into(), locked: true });
    }
    layers.push(LayerInfo { id: first, name: pimble_board::doc::FIRST_LAYER_NAME.into(), locked: false });
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
    /// size, text, style, frame or group, lock and author. Name a canvas node,
    /// or a note plus the id of a canvas in its text (`#canvas=<id>` in its
    /// picture's URL). render_canvas shows it as a picture.
    #[tool(name = "get_canvas")]
    async fn get_canvas(&self, Parameters(p): Parameters<CanvasParam>) -> Result<String, String> {
        let client = self.client().await?;
        let target = self.canvas(&client, &p.node, p.canvas.as_deref()).await?;
        self.on_canvas(&client, &target, Vec::new()).await;
        let body = self.canvas_body(&client, &target.node, target.text_canvas.as_deref()).await;
        Ok(format!("{}\nCoordinates are canvas units; x grows right, y grows down.\n{body}", target.what()))
    }

    /// See a canvas: it rendered as a PNG image, all of it, an area, or some
    /// elements (a frame shows what is in it). The answer says which canvas
    /// area the image shows and at what scale, so pixels map back to canvas
    /// coordinates.
    #[tool(name = "render_canvas")]
    async fn render_canvas(&self, Parameters(p): Parameters<RenderCanvasParams>) -> Result<CallToolResult, String> {
        let client = self.client().await?;
        let target = self.canvas(&client, &p.node, p.canvas.as_deref()).await?;
        let doc = doc_of(&target.node)?;
        let board = target.board(&doc);
        if !board.exists() {
            return Err("This canvas has not arrived on this computer yet.".into());
        }
        let plain = board.snapshot();
        let cards = self.card_titles(&client, &plain).await;
        let labels = cards.iter().map(|(link, title)| (link.clone(), CardLabel { title: title.clone(), color: None })).collect();
        let options = ViewOptions { show_all: p.show_all, card_labels: Arc::new(labels), ..ViewOptions::default() };
        let snapshot = BoardSnapshot::build_with(board.read(), options);
        // The pictures it shows, through the server like the app's loader.
        let mut pictures: HashMap<String, Arc<Picture>> = HashMap::new();
        for e in &snapshot.elements {
            if let Geom::Picture(pg) = &e.raw.geom {
                if pictures.contains_key(&pg.src) {
                    continue;
                }
                if let Some(url) = BlobUrl::parse(&pg.src) {
                    if let Ok((_, bytes)) = client.get_blob(url.store, url.blob).await {
                        if let Ok(picture) = Picture::decode(&bytes) {
                            pictures.insert(pg.src.clone(), Arc::new(picture));
                        }
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
        self.on_canvas(&client, &target, only.into_iter().collect()).await;
        let missing = snapshot
            .elements
            .iter()
            .filter_map(|e| match &e.raw.geom {
                Geom::Picture(pg) if !pictures.contains_key(&pg.src) => Some(pg.src.clone()),
                _ => None,
            })
            .count();
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
        let client = self.client().await?;
        let parent = nodes::resolve(&client, &p.parent).await?;
        let parent_node = client.get_node(parent.store, parent.node).await.map_err(rpc)?;
        let parent = match parent_node.mount_ref() {
            Some(mount) => Addr { store: mount.source_store, node: mount.source_node },
            None => parent,
        };
        // Pictures are stored as writes of the parent, as the app stores a
        // picture dropped on a row, since the canvas does not exist yet.
        let picture = match &p.picture {
            Some(file) => Some(self.store_picture(&client, parent, file).await?),
            None => None,
        };
        let prepared = self.prepare(&client, parent, &p.elements).await?;
        let author = self.board_author(&client).await;
        let now = now();
        let board = Board::standalone();
        let edited = board.edit(Stamp::new(author.as_ref(), &now), |t| build_canvas(t, picture.as_ref(), prepared, &p.elements));
        let ((ids, touched), update) = edited.into_result().map_err(|e| format!("Nothing was made: {e}."))?;
        let content = update.ok_or("Nothing was made.")?;
        let title = p
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| p.picture.as_deref().map(picture_title))
            .unwrap_or_else(|| "Canvas".to_string());
        let icon = if picture.is_some() { PHOTO_ICON } else { CANVAS_ICON };
        let custom = HashMap::from([
            (custom_keys::ICON.to_string(), serde_json::json!(icon)),
            (custom_keys::EXPLICIT_TITLE.to_string(), serde_json::json!(true)),
        ]);
        let node_id = client
            .create_node_with(parent.store, Some(parent.node), node_types::BOARD, title.clone(), Some(content), custom, Place::Last)
            .await
            .map_err(rpc)?;
        let addr = Addr { store: parent.store, node: node_id };
        let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        self.on_canvas(&client, &Target { addr, node, text_canvas: None }, touched).await;
        let stores = client.list_stores().await.map_err(rpc)?;
        let mut out = format!("Made the canvas {}.", self.describe(&client, &stores, addr).await);
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
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        if node.node_type == node_types::BOARD || node.node_type == node_types::SHEET || node.node_type == node_types::DISCUSSION || node.node_type == node_types::MOUNT {
            return Err(format!(
                "{} is a {} and has no text to put a canvas in; add_elements draws on a canvas node.",
                label(&node),
                nodes::type_name(&node)
            ));
        }
        let picture = match (&p.picture, &p.src) {
            (Some(file), None) => Some(self.store_picture(&client, addr, file).await?),
            (None, Some(src)) => Some(self.known_picture(&client, src).await?),
            (None, None) => None,
            _ => return Err("Give a picture file or a pimble-blob: src, not both.".into()),
        };
        let prepared = self.prepare(&client, addr, &p.elements).await?;
        let author = self.board_author(&client).await;
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
        self.edit_doc(&client, addr, move |doc| {
            let board = Board::in_text(doc, &canvas_for_edit);
            let edited = board.edit(Stamp::new(author.as_ref(), &now), |t| build_canvas(t, picture.as_ref(), prepared, &specs));
            *made_ref = edited.result.map_err(|e| format!("Nothing was written: {e}."))?;
            match after.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
                Some(quote) => doc.insert_markdown_after(quote, after_section, &markdown),
                None => doc.append_markdown(&markdown),
            }
            .map_err(super::crdt)?;
            Ok(())
        })
        .await?;
        let (ids, touched) = made_ids;
        let fresh = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        self.on_canvas(&client, &Target { addr, node: fresh, text_canvas: Some(canvas.clone()) }, touched).await;
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

    /// Draw on a canvas: sticky notes, text, shapes, frames, lines, arrows,
    /// connectors between elements (they follow the elements when moved), pen
    /// strokes, numbered steps, callouts, node cards, pictures and groups, in one
    /// edit. Give elements a `ref` to connect or group them in the same call.
    /// Answers each new element's id.
    #[tool(name = "add_elements")]
    async fn add_elements(&self, Parameters(p): Parameters<AddElementsParams>) -> Result<String, String> {
        if p.elements.is_empty() {
            return Err("Give at least one element.".into());
        }
        let client = self.client().await?;
        let target = self.canvas(&client, &p.node, p.canvas.as_deref()).await?;
        let prepared = self.prepare(&client, target.addr, &p.elements).await?;
        let specs = p.elements.clone();
        let layer = p.layer.clone();
        let ids = self
            .canvas_edit(&client, &target, prepared, move |t, scene, _| add_specs(t, scene, &specs, layer.as_deref()))
            .await?;
        Ok(format!("Added to {}:\n{}", target.what(), made(&p.elements, &ids)))
    }

    /// Change elements of a canvas: position, size, points, text, style,
    /// rotation, layer, frame or group, stacking order, lock. Only what a change
    /// names is written, so someone else editing another property keeps theirs.
    #[tool(name = "update_elements")]
    async fn update_elements(&self, Parameters(p): Parameters<UpdateElementsParams>) -> Result<String, String> {
        if p.changes.is_empty() {
            return Err("Give at least one change.".into());
        }
        let client = self.client().await?;
        let target = self.canvas(&client, &p.node, p.canvas.as_deref()).await?;
        let mut cards = HashMap::new();
        for (i, change) in p.changes.iter().enumerate() {
            if let Some(node) = &change.node {
                cards.insert(i, self.card_target(&client, node).await?);
            }
        }
        let changes = p.changes.clone();
        let count = changes.len();
        self.canvas_edit(&client, &target, (HashMap::new(), cards), move |t, scene, snapshot| {
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
    #[tool(name = "delete_elements")]
    async fn delete_elements(&self, Parameters(p): Parameters<DeleteElementsParams>) -> Result<String, String> {
        if p.ids.is_empty() {
            return Err("Name at least one element.".into());
        }
        let client = self.client().await?;
        let target = self.canvas(&client, &p.node, p.canvas.as_deref()).await?;
        let ids: Vec<String> = p.ids.iter().map(|s| s.trim().to_string()).collect();
        let count = ids.len();
        self.canvas_edit(&client, &target, Default::default(), move |t, scene, _| {
            for id in &ids {
                let (_, locked, _) = scene.elements.get(id).ok_or_else(|| bad(format!("there is no element \"{id}\" on this canvas")))?;
                if *locked {
                    return Err(bad(format!("element {id} is locked, or on a locked layer; unlock it first")));
                }
            }
            for id in &ids {
                t.delete_element(id)?;
            }
            Ok(())
        })
        .await?;
        Ok(format!("Deleted {count} element{} from {}.", if count == 1 { "" } else { "s" }, target.what()))
    }

    /// Add a layer to a canvas (layers organize; who made what is on each element).
    #[tool(name = "add_layer")]
    async fn add_layer(&self, Parameters(p): Parameters<AddLayerParams>) -> Result<String, String> {
        let client = self.client().await?;
        let target = self.canvas(&client, &p.node, p.canvas.as_deref()).await?;
        let name = p.name.trim().to_string();
        if name.is_empty() {
            return Err("A layer needs a name.".into());
        }
        let order = p.order.clone().unwrap_or_default();
        let id = self
            .canvas_edit(&client, &target, Default::default(), move |t, scene, _| {
                let at = placement(&order, &|l| scene.layer(l).map(|l| l.id.clone()))?;
                t.add_layer(&name, at)
            })
            .await?;
        Ok(format!("Added layer \"{}\" ({id}) to {}.", p.name.trim(), target.what()))
    }

    /// Rename, show or hide, lock or unlock, fade, or reorder a layer of a canvas.
    /// What a layer shows is the same for everyone who opens the canvas.
    #[tool(name = "update_layer")]
    async fn update_layer(&self, Parameters(p): Parameters<UpdateLayerParams>) -> Result<String, String> {
        let client = self.client().await?;
        let target = self.canvas(&client, &p.node, p.canvas.as_deref()).await?;
        let (layer, name, visible, locked, opacity, order) = (p.layer.clone(), p.name.clone(), p.visible, p.locked, p.opacity, p.order.clone());
        self.canvas_edit(&client, &target, Default::default(), move |t, scene, _| {
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
            if let Some(order) = order.as_deref() {
                let at = placement(order, &|l| scene.layer(l).map(|l| l.id.clone()))?;
                t.move_layer(&id, at)?;
            }
            Ok(())
        })
        .await?;
        Ok(format!("Changed layer \"{}\" of {}.", p.layer.trim(), target.what()))
    }

    /// Delete a layer of a canvas, with its elements, or merge it into the layer
    /// below. A canvas keeps at least one layer.
    #[tool(name = "delete_layer")]
    async fn delete_layer(&self, Parameters(p): Parameters<DeleteLayerParams>) -> Result<String, String> {
        let client = self.client().await?;
        let target = self.canvas(&client, &p.node, p.canvas.as_deref()).await?;
        let (layer, merge) = (p.layer.clone(), p.merge_down);
        self.canvas_edit(&client, &target, Default::default(), move |t, scene, _| {
            let found = scene.layer(&layer)?;
            if found.locked {
                return Err(bad(format!("layer \"{}\" is locked; update_layer can unlock it", found.name)));
            }
            let id = found.id.clone();
            if merge {
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
        let client = self.client().await?;
        let target = self.canvas(&client, &p.node, p.canvas.as_deref()).await?;
        let page = match &p.page {
            Some(spec) => Some(Page {
                rect: Rect::new(spec.x, spec.y, spec.w, spec.h).normalized(),
                fill: spec.fill.as_deref().map(|f| color(f, "fill")).transpose().map_err(|e| e.to_string())?,
            }),
            None => None,
        };
        let (fits, remove, markup) = (p.page_fits_content, p.remove_page, p.markup_visible);
        self.canvas_edit(&client, &target, Default::default(), move |t, _, snapshot| {
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
        assert!(err.to_string().contains("is locked"), "{err}");
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
        assert!(err.to_string().contains("every layer of this canvas is locked"), "{err}");
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
    fn canvas_ids_come_from_urls_too() {
        assert_eq!(canvas_id("pimble-blob:s/h#canvas=abc"), "abc");
        assert_eq!(canvas_id(" abc "), "abc");
        assert_eq!(canvas_id("pimble-blob:s/h#board=old"), "old");
        assert_eq!(picture_title("/tmp/Screenshot 1.png"), "Screenshot 1");
    }
}
