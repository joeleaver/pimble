//! Boards in panes (docs/IMAGE_MARKUP_CONTRACT.md): image nodes and
//! whiteboards, made, shown, drawn on and co-edited, on the desktop and in
//! the browser from this one module.
//!
//! A board node's document is held by the pane showing it, as an editor holds
//! a note's: one `NodeDoc` per pane, loaded from the node's bytes (or from a
//! sibling pane's copy, as an editor joins from a sibling's session), with
//! `pimble_board::Board::of_node` over it. Collaboration is the editor's shape
//! exactly (CLAUDE.md "Collaboration shape"):
//!
//! * a local edit is one `Board::edit` transaction, whose update goes out as
//!   [`BackendCommand::BroadcastChanges`] (`applyEdit`, the one write path) and
//!   to every other pane of this window holding the board, a turn later;
//! * a peer's update arrives as `BackendEvent::RemoteChanges` and is applied to
//!   the document of every pane holding the node ([`apply_remote`]);
//! * the node is subscribed once however many panes hold it, and reconciled
//!   with the server's copy when it opens and when the connection comes back.
//!
//! What a pane shows is a rinch `RenderSurface` painted by
//! `pimble_board::BoardView`, the raster cache, after rinch's `paint` example:
//! every change re-renders only what it touched, and a frame is handed to the
//! surface only when something changed. The pane's editor stays mounted and
//! hidden (docs/SPLIT_VIEW_CONTRACT.md: four editors, never re-mounted).
//!
//! The parts:
//! * here: the pane's session, the collaboration plumbing, pictures, making
//!   board nodes;
//! * [`tools`]: the pointer and the keyboard (every tool, selection, moving,
//!   resizing, rotating, typing text, arranging);
//! * [`ui`]: the board view's DOM (the surface and what floats over it, the
//!   panels: layers, meaning, frames);
//! * [`rail`]: the floating chrome (docs/DRAWING_CONTRACT.md section 4): the
//!   tool rail, the context bar, the color well, the zoom corner, its keys;
//! * [`layers`]: the layers panel, a tree of layers and groups;
//! * [`nav`]: the navigator's own surface (the map in the zoom corner);
//! * [`export`]: Copy Picture, Save Picture As, Save Original As, Flatten.
//!
//! Pictures on a board come from the same blob cache text pictures do
//! (`GetBlob`/`BlobLoaded`), decoded off the UI thread on the desktop.

mod export;
mod gradient;
mod layers;
mod nav;
mod paint;
mod paint_select;
mod points;
mod rail;
mod svg_import;
mod tools;
mod ui;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use pimble_board::{
    smooth_stroke, Align, ArrowHeads, Author, Board, BoardSnapshot, BoardTxn, BoardView, BoardWatch, CardLabel, Color, Dash,
    Geom, Kind, NewElement, Picture, PictureGeom, Placement, Point, Rect, Stamp, StartingPicture, Style, Transform, ViewOptions,
};
use pimble_core::{BlobUrl, NodeId, StoreId};
#[cfg(test)]
use pimble_core::node_types;
use pimble_crdt::NodeDoc;
use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use crate::panes::{PaneId, MAX_PANES};
use crate::protocol::{BackendCommand, NodeInit, StoredBlob};
use crate::state::{ActiveEdit, AppStore};

pub(crate) use paint::shared_cache as shared_paint_cache;
pub(crate) use tools::delete_selected;
pub(crate) use ui::render_board;

// ── Tools ───────────────────────────────────────────────────────────────────

/// The board tools (contract section 4). One set for the window, on the tool
/// rail of every canvas; they act on whichever board pane has the focus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Select,
    /// Direct select (docs/DRAWING_CONTRACT.md section 2): a path's anchors
    /// and handles.
    DirectSelect,
    Hand,
    /// The vector Pen: anchors by clicks and drags.
    Pen,
    /// Freehand, fitted to a path on release (today's pen became it,
    /// docs/DRAWING_CONTRACT.md decision 3).
    Pencil,
    /// The paint brush (docs/DRAWING_CONTRACT.md section 3), with the preset
    /// picked in the context bar.
    Brush,
    /// The brush's highlighter preset (decision 3: the highlighter became a
    /// brush preset), a face of its own in the Paint flyout: it always
    /// paints into a multiply "Highlights" layer.
    Highlighter,
    Line,
    Arrow,
    Rect,
    Ellipse,
    Diamond,
    Polygon,
    Star,
    Text,
    Step,
    Sticky,
    Connector,
    Frame,
    Eraser,
    /// Picks the color under the pointer from the composited canvas.
    Eyedropper,
    /// Flood fill in a paint layer (docs/DRAWING_CONTRACT.md section 3).
    Fill,
    /// Smudge and Blur: brushes that work the pixels of a paint layer.
    Smudge,
    Blur,
    /// Paint selections: a rectangle or a lasso of a paint layer's pixels,
    /// moved, scaled, turned or cleared.
    SelectRect,
    SelectLasso,
    /// A discussion at a point (docs/DISCUSSIONS_CONTRACT.md section 6): a
    /// pin, which is not a board element.
    Pin,
}

impl Tool {
    /// In toolbar order.
    pub const ALL: [Tool; 27] = [
        Tool::Select,
        Tool::DirectSelect,
        Tool::Hand,
        Tool::Pen,
        Tool::Pencil,
        Tool::Brush,
        Tool::Highlighter,
        Tool::Line,
        Tool::Arrow,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Diamond,
        Tool::Polygon,
        Tool::Star,
        Tool::Text,
        Tool::Step,
        Tool::Sticky,
        Tool::Connector,
        Tool::Frame,
        Tool::Eraser,
        Tool::Eyedropper,
        Tool::Fill,
        Tool::Smudge,
        Tool::Blur,
        Tool::SelectRect,
        Tool::SelectLasso,
        Tool::Pin,
    ];

    /// The tool's letter on the keyboard (contract section 4: Spectacle's
    /// where they do not collide, docs/DRAWING_CONTRACT.md's P, A and N for
    /// the Pen, Direct select and the Pencil, B, E and I for the Brush, the
    /// Eraser and the Eyedropper, G, R and Shift+R for Fill, Smudge and
    /// Blur, M and L for the paint selections; so the Rectangle is U (the
    /// shapes flyout's letter), the Polygon Y, the numbered step J, and the
    /// Highlighter and the Line are Shift+B and Shift+W, variants of the
    /// Brush and the Arrow ([`Tool::shifted`]); a board only hears plain
    /// letters while it has the keyboard, so none of them reach the app's
    /// own keys).
    pub fn key(self) -> char {
        match self {
            Tool::Select => 'v',
            Tool::DirectSelect => 'a',
            Tool::Hand => 'h',
            Tool::Pen => 'p',
            Tool::Pencil => 'n',
            Tool::Brush => 'b',
            Tool::Highlighter => 'b',
            Tool::Line => 'w',
            Tool::Arrow => 'w',
            Tool::Rect => 'u',
            Tool::Ellipse => 'o',
            Tool::Diamond => 'd',
            Tool::Polygon => 'y',
            Tool::Star => 'k',
            Tool::Text => 't',
            Tool::Step => 'j',
            Tool::Sticky => 's',
            Tool::Connector => 'c',
            Tool::Frame => 'f',
            Tool::Eraser => 'e',
            Tool::Eyedropper => 'i',
            Tool::Fill => 'g',
            Tool::Smudge => 'r',
            Tool::Blur => 'r',
            Tool::SelectRect => 'm',
            Tool::SelectLasso => 'l',
            Tool::Pin => 'q',
        }
    }

    /// Whether the tool's key is its letter with Shift: a variant of the
    /// tool the plain letter picks.
    pub fn shifted(self) -> bool {
        matches!(self, Tool::Highlighter | Tool::Line | Tool::Blur)
    }

    /// The key as a tooltip and a menu show it: "B", "Shift+R".
    pub fn key_label(self) -> String {
        let letter = self.key().to_ascii_uppercase();
        if self.shifted() {
            format!("Shift+{letter}")
        } else {
            letter.to_string()
        }
    }

    /// The tool a plain letter picks (Shift not held).
    pub fn from_key(key: &str) -> Option<Tool> {
        Tool::from_chord(key, false)
    }

    /// The tool a letter picks, with Shift held or not: Shift with a letter
    /// that has a shifted tool picks that one; otherwise Shift changes
    /// nothing (Shift+N is still the Pencil).
    pub fn from_chord(key: &str, shift: bool) -> Option<Tool> {
        let mut chars = key.chars();
        let c = chars.next()?.to_ascii_lowercase();
        if chars.next().is_some() {
            return None;
        }
        if shift {
            if let Some(tool) = Tool::ALL.into_iter().find(|t| t.shifted() && t.key() == c) {
                return Some(tool);
            }
        }
        Tool::ALL.into_iter().find(|t| !t.shifted() && t.key() == c)
    }

    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::DirectSelect => "Direct select",
            Tool::Hand => "Hand",
            Tool::Pen => "Pen",
            Tool::Pencil => "Pencil",
            Tool::Brush => "Brush",
            Tool::Highlighter => "Highlighter",
            Tool::Line => "Line",
            Tool::Arrow => "Arrow",
            Tool::Rect => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Diamond => "Diamond",
            Tool::Polygon => "Polygon",
            Tool::Star => "Star",
            Tool::Text => "Text",
            Tool::Step => "Numbered step",
            Tool::Sticky => "Sticky note",
            Tool::Connector => "Connector",
            Tool::Frame => "Frame",
            Tool::Eraser => "Eraser",
            Tool::Eyedropper => "Eyedropper",
            Tool::Fill => "Fill",
            Tool::Smudge => "Smudge",
            Tool::Blur => "Blur",
            Tool::SelectRect => "Rectangle select",
            Tool::SelectLasso => "Lasso select",
            Tool::Pin => "Discussion pin",
        }
    }

    pub fn icon(self) -> TablerIcon {
        match self {
            Tool::Select => TablerIcon::Pointer,
            Tool::DirectSelect => TablerIcon::Vector,
            Tool::Hand => TablerIcon::HandStop,
            Tool::Pen => TablerIcon::VectorBezier2,
            Tool::Pencil => TablerIcon::Pencil,
            Tool::Brush => TablerIcon::Brush,
            Tool::Highlighter => TablerIcon::Highlight,
            Tool::Line => TablerIcon::Line,
            Tool::Arrow => TablerIcon::ArrowUpRight,
            Tool::Rect => TablerIcon::Square,
            Tool::Ellipse => TablerIcon::Circle,
            Tool::Diamond => TablerIcon::Diamond,
            Tool::Polygon => TablerIcon::Hexagon,
            Tool::Star => TablerIcon::Star,
            Tool::Text => TablerIcon::Typography,
            Tool::Step => TablerIcon::CircleNumber1,
            Tool::Sticky => TablerIcon::Note,
            Tool::Connector => TablerIcon::Route,
            Tool::Frame => TablerIcon::Frame,
            Tool::Eraser => TablerIcon::Eraser,
            Tool::Eyedropper => TablerIcon::ColorPicker,
            Tool::Fill => TablerIcon::Bucket,
            Tool::Smudge => TablerIcon::HandFinger,
            Tool::Blur => TablerIcon::Blur,
            Tool::SelectRect => TablerIcon::Marquee2,
            Tool::SelectLasso => TablerIcon::Lasso,
            Tool::Pin => TablerIcon::MessagePlus,
        }
    }

    /// What the tool's style controls apply to: the kind it makes (the
    /// Pencil streams a pen stroke, which becomes a path on release). The
    /// paint tools make no element.
    pub fn kind(self) -> Option<Kind> {
        Some(match self {
            Tool::Pen => Kind::Path,
            Tool::Pencil => Kind::Pen,
            Tool::Line => Kind::Line,
            Tool::Arrow => Kind::Arrow,
            Tool::Rect => Kind::Rect,
            Tool::Ellipse => Kind::Ellipse,
            Tool::Diamond => Kind::Diamond,
            Tool::Polygon => Kind::Polygon,
            Tool::Star => Kind::Star,
            Tool::Text => Kind::Text,
            Tool::Step => Kind::Step,
            Tool::Sticky => Kind::Sticky,
            Tool::Connector => Kind::Connector,
            Tool::Frame => Kind::Frame,
            Tool::Select
            | Tool::DirectSelect
            | Tool::Hand
            | Tool::Eraser
            | Tool::Pin
            | Tool::Brush
            | Tool::Highlighter
            | Tool::Eyedropper
            | Tool::Fill
            | Tool::Smudge
            | Tool::Blur
            | Tool::SelectRect
            | Tool::SelectLasso => return None,
        })
    }

    /// The tools that paint with a brush (docs/DRAWING_CONTRACT.md section
    /// 3): the brush cursor follows the pointer, `[` and `]` resize, Alt
    /// picks a color with the brushes that lay one down (the eraser's,
    /// the smudge's and the blur's Alt does nothing).
    pub fn paints(self) -> bool {
        matches!(self, Tool::Brush | Tool::Highlighter | Tool::Eraser | Tool::Smudge | Tool::Blur)
    }

    /// The paint selections (rectangle and lasso).
    pub fn selects_paint(self) -> bool {
        matches!(self, Tool::SelectRect | Tool::SelectLasso)
    }
}

/// The marks' first color: the marks' red.
pub const DEFAULT_COLOR: &str = "#e53935";
/// The pen's first width, in board units.
pub const DEFAULT_WIDTH: f32 = 4.0;
/// The first font size, in board units.
pub const DEFAULT_FONT_SIZE: f32 = 24.0;
/// The quick colors the toolbar offers (the marks' red first, then the
/// appearance palette's, then black and white).
pub(crate) fn palette() -> Vec<String> {
    let mut colors = vec![DEFAULT_COLOR.to_string()];
    colors.extend(crate::appearance::COLOR_CHOICES.iter().map(|(_, c)| c.to_string()));
    colors.push("#212121".to_string());
    colors.push("#ffffff".to_string());
    colors
}
/// The widths the style panel offers (thin to extra thick), board units.
pub(crate) const WIDTHS: [f32; 4] = [2.0, 4.0, 8.0, 16.0];
/// The font sizes the style panel offers (S, M, L, XL), board units.
pub(crate) const FONT_SIZES: [(&str, f32); 4] = [("S", 16.0), ("M", 24.0), ("L", 36.0), ("XL", 56.0)];
/// The grid's spacing, in board units (contract section 4, "grid").
pub(crate) const GRID: f32 = 20.0;

/// The style new elements take, and what the context bar shows: one set for
/// the window, like the tool. Selecting elements loads the first one's style
/// here; changing a value writes it to every selected element too.
#[derive(Clone, Copy)]
pub struct StyleUi {
    pub stroke: Signal<String>,
    /// `None`: no fill.
    pub fill: Signal<Option<String>>,
    pub width: Signal<f32>,
    pub opacity: Signal<f32>,
    pub dash: Signal<Dash>,
    pub font_size: Signal<f32>,
    pub align: Signal<Align>,
    /// The arrow tool's heads (the line tool draws none).
    pub heads: Signal<ArrowHeads>,
    /// Per viewer (contract section 4): the grid shown, and snapping to it.
    pub grid: Signal<bool>,
    pub snap: Signal<bool>,
    /// The navigator (the map in the bottom right corner) shown on every
    /// board.
    pub map: Signal<bool>,
    /// Each tool rail flyout's face: the tool last used from it
    /// (docs/DRAWING_CONTRACT.md section 4), by flyout, for this session.
    pub faces: Signal<Vec<Tool>>,
    /// Which square of the color well is in front (the one a picked color
    /// goes to).
    pub front: Signal<rail::Well>,
    /// The colors last picked, newest first.
    pub recent: Signal<Vec<String>>,
    /// Stroke ends and corners (`None`: the kind's own, round).
    pub cap: Signal<Option<pimble_board::Cap>>,
    pub join: Signal<Option<pimble_board::Join>>,
    /// The miter limit, for mitered corners.
    pub miter: Signal<f32>,
    /// A custom dash pattern as typed (`"6 3 1 3"`, board units); empty for
    /// the dash preset.
    pub dash_text: Signal<String>,
    /// A rectangle's corner radius, board units.
    pub radius: Signal<f32>,
    /// A polygon's sides.
    pub sides: Signal<u32>,
    /// A star's points, and its inner radius as a fraction of its outer.
    pub points: Signal<u32>,
    pub inner: Signal<f32>,
    /// The fill rule of a selected path.
    pub rule: Signal<pimble_board::PathRule>,
    /// The Pencil's fitting tolerance, logical pixels at the zoom it is
    /// drawn at (`rail::SMOOTHING`).
    pub smoothing: Signal<f32>,
    /// The fill's and the stroke's gradient (`None`: a solid color, the
    /// `fill`/`stroke` above), what new elements take and the selection
    /// shows (docs/DRAWING_CONTRACT.md, "Built in wave 3 (UI)").
    pub fill_gradient: Signal<Option<pimble_board::Gradient>>,
    pub stroke_gradient: Signal<Option<pimble_board::Gradient>>,
    /// The stop picked on the well's stop bar.
    pub stop: Signal<usize>,
    /// The Brush's preset, by name (the highlighter's is the Highlighter
    /// tool's own).
    pub brush: Signal<&'static str>,
    /// Each preset's settings as the context bar left them, in
    /// `pimble_board::PRESETS` order (docs/DRAWING_CONTRACT.md, "Built in
    /// wave 4 (UI)").
    pub brushes: Signal<Vec<pimble_board::BrushSettings>>,
    /// The eraser's size in paint layers, board units.
    pub eraser_size: Signal<f32>,
    /// The Fill's tolerance (0 to 1) and whether it fills only what
    /// touches the seed (docs/DRAWING_CONTRACT.md, "Built in wave 5 (UI)").
    pub tolerance: Signal<f32>,
    pub contiguous: Signal<bool>,
    /// Smudge's and Blur's size, board units, and their strength (0 to 1).
    pub retouch_size: Signal<f32>,
    pub strength: Signal<f32>,
    /// Text's font: family, bold, italic (docs/DRAWING_WAVE6_CONTRACT.md
    /// section 4, "Built: map lettering").
    pub font: Signal<pimble_board::FontFamily>,
    pub bold: Signal<bool>,
    pub italic: Signal<bool>,
    /// Letter spacing in em.
    pub tracking: Signal<f32>,
    /// The halo's width as a fraction of the font size (0: none; written as
    /// board units, `rail::HALOS`) and its color.
    pub halo: Signal<f32>,
    pub halo_color: Signal<String>,
    /// The first selected element is a text set along a path: where along
    /// it (0 to 1) and on which side.
    pub on_path: Signal<bool>,
    pub along_offset: Signal<f32>,
    pub along_side: Signal<pimble_board::AlongSide>,
}

impl StyleUi {
    pub fn new() -> Self {
        Self {
            stroke: Signal::new(DEFAULT_COLOR.to_string()),
            fill: Signal::new(None),
            width: Signal::new(DEFAULT_WIDTH),
            opacity: Signal::new(1.0),
            dash: Signal::new(Dash::Solid),
            font_size: Signal::new(DEFAULT_FONT_SIZE),
            align: Signal::new(Align::Left),
            heads: Signal::new(ArrowHeads::End),
            grid: Signal::new(false),
            snap: Signal::new(false),
            map: Signal::new(true),
            faces: Signal::new(rail::default_faces()),
            front: Signal::new(rail::Well::Stroke),
            recent: Signal::new(Vec::new()),
            cap: Signal::new(None),
            join: Signal::new(None),
            miter: Signal::new(pimble_board::DEFAULT_MITER),
            dash_text: Signal::new(String::new()),
            radius: Signal::new(0.0),
            sides: Signal::new(pimble_board::DEFAULT_SIDES),
            points: Signal::new(pimble_board::DEFAULT_STAR_POINTS),
            inner: Signal::new(pimble_board::DEFAULT_STAR_INNER),
            rule: Signal::new(pimble_board::PathRule::NonZero),
            smoothing: Signal::new(rail::DEFAULT_SMOOTHING),
            fill_gradient: Signal::new(None),
            stroke_gradient: Signal::new(None),
            stop: Signal::new(0),
            brush: Signal::new(pimble_board::brush::ROUND.name),
            brushes: Signal::new(paint::default_brushes()),
            eraser_size: Signal::new(paint::ERASER_SIZE),
            tolerance: Signal::new(pimble_board::paint_ops::DEFAULT_TOLERANCE),
            contiguous: Signal::new(true),
            retouch_size: Signal::new(paint::RETOUCH_SIZE),
            strength: Signal::new(pimble_board::paint_ops::DEFAULT_STRENGTH),
            font: Signal::new(pimble_board::FontFamily::Sans),
            bold: Signal::new(false),
            italic: Signal::new(false),
            tracking: Signal::new(0.0),
            halo: Signal::new(0.0),
            halo_color: Signal::new("#ffffff".to_string()),
            on_path: Signal::new(false),
            along_offset: Signal::new(0.0),
            along_side: Signal::new(pimble_board::AlongSide::Above),
        }
    }
}

impl Default for StyleUi {
    fn default() -> Self {
        Self::new()
    }
}

/// One layer as the layers panel shows it, in the panel's order (top
/// first, a group right before what it holds).
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRow {
    pub id: String,
    pub name: String,
    /// Its own flags.
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    /// Elements in it (none for a group).
    pub count: usize,
    pub kind: pimble_board::LayerKind,
    pub blend: pimble_board::Blend,
    /// The group it is read inside.
    pub parent: Option<String>,
    pub depth: usize,
    /// It shows (it and every group above it are visible).
    pub shown: bool,
    /// It is locked, by its own flag or a group's.
    pub held: bool,
    /// Layers directly inside it (a group's).
    pub children: usize,
    /// Its clip, as the row's tooltip says it (docs/DRAWING_WAVE6_CONTRACT.md
    /// section 1): what clips it, or why the clip it names is not in effect.
    pub clip: Option<String>,
    /// That clip is in effect.
    pub clipped: bool,
}

impl LayerRow {
    pub fn is_group(&self) -> bool {
        self.kind.is_group()
    }
}

/// The one popup a board's floating chrome has open at a time: a rail
/// flyout, the color well's picker, a menu of the context bar or the zoom
/// corner, the layers panel's blend list. A press on the canvas closes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Popup {
    #[default]
    None,
    Flyout(usize),
    Well,
    /// The context bar's "more" menu: forward, backward, front, back.
    Arrange,
    Align,
    Export,
    Blend,
    /// The context bar's pickers, each a row under the bar.
    Width,
    Dash,
    Heads,
    Font,
    TextAlign,
    Opacity,
    /// A live shape's parameters: corner radius, sides, star points and
    /// inner radius.
    Shape,
    /// Stroke ends and corners, and a path's fill rule.
    Detail,
    /// The Pencil's smoothing.
    Smoothing,
    /// Combining two or more elements: union, subtract, intersect, exclude.
    Combine,
    /// The brush's preset, size, opacity, flow and hardness.
    Preset,
    BrushSize,
    BrushOpacity,
    Flow,
    Hardness,
    /// The Fill's tolerance, and contiguous or everywhere.
    Tolerance,
    FillMode,
    /// Smudge's and Blur's strength.
    Strength,
}

/// What the "Meaning" section is about: the one selected element, or else
/// the picked layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeaningTarget {
    Element(String),
    Layer(String),
}

/// The "Meaning" section's view of its target: what it is called, its note
/// and its named fields (docs/IMAGE_MARKUP_CONTRACT.md, "Meaning").
#[derive(Debug, Clone, PartialEq)]
pub struct MeaningView {
    pub target: MeaningTarget,
    /// "Sticky note: Buy milk", "Layer: Walls".
    pub heading: String,
    pub note: String,
    pub fields: Vec<(String, String)>,
}

/// The box a text being typed sits in over the board, logical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub font_px: f32,
    pub color: String,
    pub background: String,
    pub align: Align,
}

/// What a pane's board view shows over its surface, and its floating chrome and panels, as
/// signals (logical pixels).
#[derive(Clone, Copy)]
pub struct PaneUi {
    /// Every selected element's outline.
    pub outlines: Signal<Vec<(f32, f32, f32, f32)>>,
    /// The selection's box, where the handles go; `None` without a
    /// selection that can be resized.
    pub handles: Signal<Option<(f32, f32, f32, f32)>>,
    /// How many elements are selected.
    pub selected: Signal<usize>,
    /// The marquee being dragged.
    pub marquee: Signal<Option<(f32, f32, f32, f32)>>,
    /// The layers, top first.
    pub layers: Signal<Vec<LayerRow>>,
    /// The layer new elements go into.
    pub layer: Signal<Option<String>>,
    /// The layer being renamed in place, and the name being typed.
    pub renaming: Signal<Option<String>>,
    pub rename_text: Signal<String>,
    /// The frames, for the navigator: (id, name).
    pub frames: Signal<Vec<(String, String)>>,
    /// The attribution under the pointer (contract section 6): where, and
    /// the sentence.
    pub tip: Signal<Option<(f32, f32, String)>>,
    /// The text being typed: its box, and what the input is given to hold
    /// (only when typing starts or a collaborator's typing merges in, never
    /// echoed back per keystroke: a write racing fast typing drops letters),
    /// with a count so the same text given twice is written twice.
    pub text_box: Signal<Option<TextBox>>,
    pub text_value: Signal<(u64, String)>,
    /// "Color by author" (contract section 6), a way of looking, per pane.
    pub by_author: Signal<bool>,
    /// The pointer is over the board (the markup control shows).
    pub hover: Signal<bool>,
    /// The navigator: the view's rectangle over it, logical pixels.
    pub nav_view: Signal<Option<(f32, f32, f32, f32)>>,
    /// Distinct authors on the board.
    pub authors: Signal<usize>,
    /// The node cards on the board: the node each links to, and the link.
    pub cards: Signal<Vec<(StoreId, NodeId, String)>>,
    /// The board has a picture selected (Save Original As...).
    pub picture_selected: Signal<bool>,
    /// The discussions pinned to the board, where they are on screen
    /// (logical pixels), numbered in the order they were made.
    pub pins: Signal<Vec<PinView>>,
    /// Everyone else's pointers and selections on the board
    /// (docs/PRESENCE_CONTRACT.md section 5).
    pub others: Signal<crate::presence::board::Others>,
    /// The "Meaning" section: the selected element's or the picked layer's
    /// note and fields.
    pub meaning: Signal<Option<MeaningView>>,
    /// What the note box is given to hold: only when its target changes or
    /// someone else's edit of the note arrives, never echoed per keystroke
    /// (as `text_value`), with a count so the same text given twice is.
    pub note_value: Signal<(u64, String)>,
    /// The field being edited in place: its name and what is typed.
    pub field_edit: Signal<(String, String)>,
    /// The new field being typed: its name and value.
    pub new_field: Signal<(String, String)>,
    /// The kinds of the selected elements (what the context bar offers).
    pub selected_kinds: Signal<Vec<Kind>>,
    /// The zoom, in percent of one board unit to a CSS pixel.
    pub zoom: Signal<f32>,
    /// The popup open over the board.
    pub popup: Signal<Popup>,
    /// Tab hid the panels.
    pub panels_hidden: Signal<bool>,
    /// The panels folded to their title rows: layers, meaning, frames.
    pub folded: Signal<[bool; 3]>,
    /// The layer groups folded in the layers panel (this viewer's own,
    /// never the document's).
    pub folded_groups: Signal<Vec<String>>,
    /// Layers picked besides `layer` (Ctrl or Shift clicks), for Group
    /// Layers.
    pub picked_layers: Signal<Vec<String>>,
    /// Where the last press on the board was, which says who hears keys
    /// the canvas's surface no longer does.
    pub keys: Signal<rail::Keys>,
    /// A layer row being dragged over: the row and where it would land.
    pub drop_at: Signal<Option<(String, layers::DropZone)>>,
    /// Each layer's thumbnail, a `data:` URL, by layer id (drawn again a
    /// moment after the layer changes, `paint::thumbs_due`).
    pub thumbs: Signal<HashMap<String, String>>,
    /// A Flatten or a merge drawn as pixels is under way (drawn, then its
    /// picture stored): what the layers panel says meanwhile.
    pub rasterizing: Signal<Option<&'static str>>,
}

impl PaneUi {
    pub fn new() -> Self {
        Self {
            outlines: Signal::new(Vec::new()),
            pins: Signal::new(Vec::new()),
            others: Signal::new(Default::default()),
            handles: Signal::new(None),
            selected: Signal::new(0),
            marquee: Signal::new(None),
            layers: Signal::new(Vec::new()),
            layer: Signal::new(None),
            renaming: Signal::new(None),
            rename_text: Signal::new(String::new()),
            frames: Signal::new(Vec::new()),
            tip: Signal::new(None),
            text_box: Signal::new(None),
            text_value: Signal::new((0, String::new())),
            by_author: Signal::new(false),
            hover: Signal::new(false),
            nav_view: Signal::new(None),
            authors: Signal::new(0),
            cards: Signal::new(Vec::new()),
            picture_selected: Signal::new(false),
            meaning: Signal::new(None),
            note_value: Signal::new((0, String::new())),
            field_edit: Signal::new((String::new(), String::new())),
            new_field: Signal::new((String::new(), String::new())),
            selected_kinds: Signal::new(Vec::new()),
            zoom: Signal::new(100.0),
            popup: Signal::new(Popup::None),
            panels_hidden: Signal::new(false),
            folded: Signal::new([false; 3]),
            folded_groups: Signal::new(Vec::new()),
            picked_layers: Signal::new(Vec::new()),
            keys: Signal::new(rail::Keys::Canvas),
            drop_at: Signal::new(None),
            thumbs: Signal::new(HashMap::new()),
            rasterizing: Signal::new(None),
        }
    }
}

impl Default for PaneUi {
    fn default() -> Self {
        Self::new()
    }
}

/// How long after the last wheel step the cache is re-rendered at the new zoom.
const SETTLE_MS: u32 = 150;
/// How often a stroke being drawn is appended to the document (contract
/// section 3: others watch it being drawn).
const APPEND_EVERY_MS: f64 = 100.0;
/// How often a dragged element's position is written while it moves.
const DRAG_WRITE_EVERY_MS: f64 = 80.0;
/// `PutBlob` request ids of image nodes start here, clear of the ones
/// `pictures.rs` numbers for pictures in text.
const REQUEST_BASE: u64 = 1 << 40;

/// "New Canvas from Picture..." on a store that refuses pictures, or with nothing picked.
const NOT_A_PICTURE: &str = "That file is not a picture Pimble can show.";
/// A tool refused on a locked layer or element (a guard against accidents,
/// contract section 5, not a permission): `pimble-board`'s sentence.
const LOCKED: &str = pimble_board::locks::ELEMENT_LOCKED;

// ── Who is drawing ──────────────────────────────────────────────────────────

thread_local! {
    /// The signed-in account (contract section 6), or nobody.
    static AUTHOR: RefCell<Option<Author>> = const { RefCell::new(None) };
}

/// Who board edits are stamped as: the signed-in Pimble Cloud account (its
/// `sub` and its email's local part), or nobody when nobody is signed in.
/// The desktop calls this from `CloudStatusChanged`, the browser once its
/// session is unlocked.
pub fn set_author(user_id: Option<String>, email: Option<String>) {
    let author = match (user_id, email) {
        (Some(id), email) if !id.is_empty() => {
            let name = email.as_deref().and_then(|e| e.split('@').next()).filter(|n| !n.is_empty()).unwrap_or(id.as_str()).to_string();
            Some(Author::new(id.clone(), name))
        }
        _ => None,
    };
    tracing::info!("board edits are stamped as {:?}", author.as_ref().map(|a| &a.name));
    AUTHOR.with(|a| *a.borrow_mut() = author);
}

/// The name the account goes by where no share names it otherwise (its
/// default name, docs/PRESENCE_CONTRACT.md section 3), once the accounts
/// service has said it: stamps stop carrying the email's local part.
pub fn set_author_name(name: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    AUTHOR.with(|a| {
        if let Some(author) = a.borrow_mut().as_mut() {
            author.name = name.to_string();
        }
    });
}

fn author() -> Option<Author> {
    AUTHOR.with(|a| a.borrow().clone())
}

/// Who board edits made outside a pane's board view are stamped as.
pub(crate) fn current_author() -> Option<Author> {
    author()
}

/// Milliseconds since the epoch, on both targets.
pub(crate) fn now_ms() -> f64 {
    #[cfg(feature = "native")]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
    }
    #[cfg(not(feature = "native"))]
    {
        js_sys::Date::now()
    }
}

/// A board timestamp: RFC 3339 UTC with milliseconds, the one shape every
/// writer uses (steps sort by it as a string).
pub(crate) fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A per-person undo manager's options (contract section 4), with a clock
/// that works in the browser too.
/// The undo manager's options. `held` is the session's held clock: while it
/// is not zero the manager reads that time, so every edit made meanwhile
/// joins one undo step however long it takes (a Pen path drawn anchor by
/// anchor, a point dragged with pauses); zero is the real clock.
fn undo_options(held: Arc<std::sync::atomic::AtomicU64>) -> yrs::undo::Options<()> {
    yrs::undo::Options {
        capture_timeout_millis: 500,
        tracked_origins: Default::default(),
        capture_transaction: None,
        timestamp: Arc::new(move || match held.load(std::sync::atomic::Ordering::Relaxed) {
            0 => now_ms() as u64,
            t => t,
        }),
        init_undo_stack: Vec::new(),
        init_redo_stack: Vec::new(),
    }
}

// ── A pane's board ──────────────────────────────────────────────────────────

/// An element as a gesture found it: what the gesture writes is computed from
/// this, never from the moving document.
#[derive(Debug, Clone)]
struct Base {
    id: String,
    kind: Kind,
    geom: Geom,
    transform: Transform,
    /// Its own geometry's extent (the pivot of its rotation and scale),
    /// element space.
    pivot: Rect,
    /// Its parent's world transform as a translation (frames and groups are
    /// moved, not rotated, by the tools).
    parent_offset: (f32, f32),
    /// Its world transform is a plain translation (no rotation or scale
    /// anywhere above it): its geometry can be edited directly.
    plain: bool,
    /// Its bounds in board units.
    bounds: Rect,
}

/// A resize or rotate handle of the selection box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
    Rotate,
}

/// What the pointer is doing on a board.
enum Gesture {
    Idle,
    /// Middle-drag, space-drag or the hand tool: the last position, physical
    /// pixels.
    Pan { last: (f32, f32) },
    /// A pen or highlighter stroke: its element once it is written (its
    /// first ~100 ms of points make it, so a stroke cancelled at once, a
    /// finger that turns out to be the first of a pinch, writes nothing),
    /// what it is made with, and the points not yet appended.
    Stroke { id: Option<String>, kind: Kind, style: Style, layer: String, pending: Vec<[f32; 3]>, appended_at: f64, pencil: bool },
    /// A line, arrow, shape or frame being dragged out from `start`: its
    /// element once the drag has gone far enough to make one.
    Shape { kind: Kind, start: Point, last: Point, id: Option<String>, written_at: f64 },
    /// A connector being dragged from `from`.
    Connector { from: pimble_board::End, last: Point, id: Option<String>, written_at: f64 },
    /// Moving the selection.
    Move { start: Point, last: Point, bases: Vec<Base>, written_at: f64, clicked: Option<String> },
    /// Resizing the selection by a handle.
    Resize { handle: Handle, bounds: Rect, last: Point, bases: Vec<Base>, written_at: f64 },
    /// Rotating the selection about `center`.
    Rotate { center: Point, start: f32, last: f32, bases: Vec<Base>, written_at: f64 },
    /// A marquee from `start` (physical pixels); `base` is the selection it
    /// adds to (shift).
    Marquee { start: (f32, f32), now: (f32, f32), base: Vec<String> },
    /// The eraser, held down.
    Erase,
    /// Two fingers down: their moves pan, their spread zooms (`Pinch`),
    /// until the last of them is lifted.
    Pinch,
    /// The Pen's press, placing an anchor (or closing): the path is the
    /// session's `pen`.
    PenPlace,
    /// Direct select dragging anchors or a handle of the session's
    /// `points` element.
    Points(points::Drag),
    /// Direct select's marquee over anchors, from `start` (physical
    /// pixels); `base` is the anchor selection it adds to.
    PointMarquee { start: (f32, f32), now: (f32, f32), base: Vec<(String, Vec<pimble_board::AnchorRef>)> },
    /// Dragging a handle of the selected element's gradient.
    GradientHandle(gradient::HandleDrag),
    /// A paint stroke (the Brush, the Highlighter, the Eraser in a paint
    /// layer) being drawn.
    Paint(paint::Stroke),
    /// The Eyedropper (or Alt with a brush) held down: the color under the
    /// pointer is picked as it moves.
    Eyedrop,
    /// A paint selection being drawn: a rectangle from `start` to `now`, or
    /// a lasso through `lasso`'s points (board units), of paint layer
    /// `layer`.
    PaintSelect { layer: String, start: Point, now: Point, lasso: Option<Vec<Point>> },
    /// The paint selection's pixels dragged: moved, scaled by a handle or
    /// turned.
    PaintSelDrag(paint_select::SelDrag),
}

/// A text being typed into an element (contract section 4: "typed in a real
/// input overlaid on the surface"). `id` is `None` until the first character
/// makes the element.
struct TextEdit {
    id: Option<String>,
    kind: Kind,
    at: Point,
    /// What the input held after the last keystroke.
    last: String,
}

/// One pane's board: the node's document, the board in it, and the view.
struct Session {
    store_id: StoreId,
    node_id: NodeId,
    doc: NodeDoc,
    board: Board,
    /// `None` until the board exists here (its creating update may still be
    /// on its way).
    watch: Option<BoardWatch>,
    undo: Option<yrs::undo::UndoManager<()>>,
    view: BoardView,
    /// Fitted to the visible extent once, the first time it showed a board.
    fitted: bool,
    /// The camera this device left the board at (`views.rs`), taken instead
    /// of fitting the first time it shows.
    restore: Option<crate::views::Camera>,
    gesture: Gesture,
    /// The selection, in the order it was made.
    selected: Vec<String>,
    /// The space bar is held: a left drag pans.
    space: bool,
    /// Shift is held: constrain, add to the selection.
    shift: bool,
    /// Bumped by every wheel step; the settle timer of an older one does nothing.
    settle_generation: u64,
    /// The last press: when, where (physical pixels), for double-clicks.
    last_press: (f64, f32, f32),
    /// The pointer's last position over the surface, physical pixels.
    pointer: (f32, f32),
    /// The pointer is over the surface (its frames say where).
    over: bool,
    /// The pointer and button whose press the gesture is (one at a time:
    /// another pointer's or button's presses meanwhile start nothing).
    driver: Option<(u64, SurfaceMouseButton)>,
    /// The fingers on the surface, in the order they came down, with where
    /// they are (physical pixels).
    touches: Vec<(u64, (f32, f32))>,
    /// The pressure of the pointer drawing (0.5 for a mouse).
    pressure: f32,
    text: Option<TextEdit>,
    /// The way of looking (color by author, card titles).
    options: ViewOptions,
    /// Bumped by every change; the navigator's timer of an older one does
    /// nothing.
    nav_generation: u64,
    /// The tool this board opens on has been picked (once its board is here).
    tool_chosen: bool,
    /// The board view is over a note's text, marking up one of its pictures
    /// (contract section 2): the board is `data["board:<id>"]` of the note.
    markup: Option<Markup>,
    /// Alt is held (the Pen and Direct select break a handle's link).
    alt: bool,
    /// The Pen's path being drawn.
    pen: Option<points::PenSession>,
    /// The element Direct select shows the points of.
    points: Option<points::PointEdit>,
    /// The undo manager's held clock ([`undo_options`]).
    clock: Arc<std::sync::atomic::AtomicU64>,
    /// Paint edits made here and not sent yet, and when the last went
    /// (`paint::flush`).
    outbox: Vec<Vec<u8>>,
    outbox_at: f64,
    /// Bumped by every change to the board (the thumbnails wait for it to
    /// settle).
    version: u64,
    /// The pressure a paint tool paints with (1 for a mouse).
    paint_pressure: f32,
    /// The paint selection (rectangle or lasso) and what was done to its
    /// pixels so far.
    paint_sel: Option<paint_select::PaintSel>,
    /// How far the selection's ants have marched.
    ants: f32,
}

/// A picture in text being marked up in a pane.
struct Markup {
    /// The picture's `src`, by which its atom is found again.
    src: String,
    /// Where the atom was when the view opened (the nearest such picture to
    /// it is the one).
    pos: usize,
    board_id: String,
    /// The board, made in this pane's copy of the note when the view opened
    /// so the picture shows, and sent with the first mark ("the first mark
    /// mints the board id"): opening and closing the view writes nothing.
    pending: Option<Vec<u8>>,
    /// The atom's `board` is still to be written (with the first mark).
    attr_needed: bool,
}

impl Session {
    fn snapshot(&self) -> &BoardSnapshot {
        self.view.snapshot()
    }

    /// Bring the view up to date with the document after any change to it:
    /// only what changed is re-read, once the board exists.
    fn sync_view(&mut self) {
        match &self.watch {
            Some(watch) => {
                let changes = watch.take();
                if !changes.is_empty() {
                    self.version += 1;
                }
                self.view.update(&self.board, &changes);
            }
            None => {
                if let Some(watch) = self.board.watch() {
                    self.watch = Some(watch);
                    let snapshot = self.board.snapshot().with_options(self.options.clone(), &self.board);
                    self.view.set_snapshot(snapshot);
                    self.version += 1;
                }
            }
        }
        if self.undo.is_none() {
            self.undo = self.board.undo_manager(undo_options(self.clock.clone()));
        }
        let snapshot = self.view.snapshot();
        self.selected.retain(|id| snapshot.get(id).is_some());
    }

    /// A board rectangle on screen, physical pixels (left, top, width, height).
    fn to_screen(&self, r: &Rect) -> (f32, f32, f32, f32) {
        let (x0, y0) = self.view.board_to_screen(Point::new(r.x, r.y));
        let (x1, y1) = self.view.board_to_screen(Point::new(r.right(), r.bottom()));
        (x0, y0, x1 - x0, y1 - y0)
    }

    /// The board rectangle the view shows.
    fn visible_rect(&self) -> Rect {
        let (w, h) = self.view.size();
        let a = self.view.screen_to_board(0.0, 0.0);
        let b = self.view.screen_to_board(w as f32, h as f32);
        Rect::new(a.x.min(b.x), a.y.min(b.y), (b.x - a.x).abs(), (b.y - a.y).abs())
    }

    /// The union of the selected elements' bounds.
    fn selection_bounds(&self) -> Option<Rect> {
        let snapshot = self.snapshot();
        self.selected.iter().filter_map(|id| snapshot.get(id)).map(|e| e.bounds).reduce(|a, b| a.union(&b))
    }

    /// Whether element `id` may be changed by the tools: neither it nor its
    /// layer is locked (contract section 5, judged by `pimble-board`).
    fn editable(&self, id: &str) -> bool {
        self.snapshot().editable(id)
    }
}

/// A discussion pinned to a board, as the board view draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct PinView {
    pub store_id: StoreId,
    pub node_id: NodeId,
    pub x: f32,
    pub y: f32,
    pub number: usize,
    pub title: String,
    pub count: usize,
    pub unread: bool,
}

/// The discussions pinned to some board changed: every board pane draws its
/// pins again.
pub(crate) fn pins_changed(store: AppStore) {
    for pane in PaneId::ALL {
        if with_session(pane, |_| ()).is_some() {
            after_change(store, pane);
        }
    }
}

thread_local! {
    static SESSIONS: RefCell<[Option<Session>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// Each pane slot's surface, made once (the slot is mounted once).
    static SURFACES: RefCell<[Option<RenderSurfaceHandle>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// Each surface's box in logical pixels, for the desktop's pixel scale
    /// (pointer events are logical, frames physical).
    static BOUNDS: RefCell<[Option<Signal<rinch::core::ElementBounds>>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// The app, for the callbacks rinch calls with nothing in hand.
    static APP: Cell<Option<AppStore>> = const { Cell::new(None) };
    /// The tree's drag (a row's tree value), so a row dropped on a board
    /// becomes a node card (contract section 4).
    static TREE_DRAG: Cell<Option<DragContext<String>>> = const { Cell::new(None) };
}

/// The tree's drag-and-drop context, handed over once by the app.
pub(crate) fn set_tree_drag(ctx: DragContext<String>) {
    TREE_DRAG.with(|d| d.set(Some(ctx)));
}

/// The tree row being dragged (its tree value), when a drop takes it.
/// `take` decides whether it is taken: a drop that does not want it leaves
/// it to whoever else might.
pub(crate) fn take_tree_drag(take: impl FnOnce(&str) -> bool) -> bool {
    let Some(ctx) = TREE_DRAG.with(Cell::get) else { return false };
    let Some(value) = ctx.get() else { return false };
    if !take(&value) {
        return false;
    }
    ctx.take();
    true
}

fn with_session<R>(pane: PaneId, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()].as_mut().map(f))
}

fn surface_of(pane: PaneId) -> Option<RenderSurfaceHandle> {
    SURFACES.with(|s| s.borrow()[pane.index()].clone())
}

/// Logical to physical pixels on pane `pane`'s surface.
fn scale_of(pane: PaneId) -> f32 {
    #[cfg(not(feature = "native"))]
    {
        let _ = pane;
        web_sys::window().map(|w| w.device_pixel_ratio() as f32).filter(|s| *s > 0.0).unwrap_or(1.0)
    }
    #[cfg(feature = "native")]
    {
        let physical = surface_of(pane).map(|s| s.layout_size().0).unwrap_or(0);
        let logical = BOUNDS.with(|b| b.borrow()[pane.index()]).map(|sig| untracked(|| sig.get().width)).unwrap_or(0.0);
        if physical > 0 && logical > 0.0 {
            physical as f32 / logical
        } else {
            1.0
        }
    }
}

/// The board's background on the current theme.
fn background(dark: bool) -> Color {
    if dark {
        Color::rgb(0x1f, 0x20, 0x24)
    } else {
        Color::rgb(0xf8, 0xf9, 0xfa)
    }
}

/// Start showing board node `node_id` in pane `pane`: hold its document,
/// subscribe to it (once per node), and reconcile with the server's copy.
/// `content` is the node's document as the cache has it; another pane of
/// this window holding the node is the fresher copy and is joined instead.
pub(crate) fn start(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, content: &[u8]) {
    // Whatever the pane held goes first: its editor's session, or a board.
    crate::editor::stop_editing(store, pane);
    APP.with(|app| app.set(Some(store)));
    let first = !store.is_editing(store_id, node_id);
    let sibling = store
        .panes_boarding(store_id, node_id)
        .into_iter()
        .find_map(|other| with_session(other, |s| s.doc.save()));
    let bytes = sibling.as_deref().unwrap_or(content);
    let doc = match NodeDoc::load(bytes) {
        Ok(doc) => doc,
        Err(e) => {
            tracing::warn!("board {node_id} could not be read ({e}); showing it empty until the server's copy arrives");
            NodeDoc::new()
        }
    };
    let board = Board::of_node(&doc).with_origin(format!("pimble-board-pane-{}", pane.index()));
    // Its pins are the discussions on it (docs/DISCUSSIONS_CONTRACT.md section 6).
    store.send(BackendCommand::ListAttachments { store_id, node_id });
    let dark = untracked(|| store.dark_mode.get());
    let ui = store.pane(pane).board_ui;
    let mut view = BoardView::new(1, 1, background(dark));
    view.set_paint_cache(paint::shared_cache());
    if untracked(|| store.board_style.grid.get()) {
        view.set_grid(Some(GRID));
    }
    let mut session = Session {
        store_id,
        node_id,
        doc,
        board,
        watch: None,
        undo: None,
        view,
        fitted: false,
        restore: crate::views::camera(store_id, node_id),
        gesture: Gesture::Idle,
        selected: Vec::new(),
        space: false,
        shift: false,
        settle_generation: 0,
        last_press: (0.0, 0.0, 0.0),
        pointer: (0.0, 0.0),
        over: false,
        driver: None,
        touches: Vec::new(),
        pressure: 0.5,
        text: None,
        options: ViewOptions { color_by_author: untracked(|| ui.by_author.get()), ..ViewOptions::default() },
        nav_generation: 0,
        tool_chosen: false,
        markup: None,
        alt: false,
        pen: None,
        points: None,
        clock: Arc::default(),
        outbox: Vec::new(),
        outbox_at: 0.0,
        version: 0,
        paint_pressure: 1.0,
        paint_sel: None,
        ants: 0.0,
    };
    session.sync_view();
    SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()] = Some(session));
    let state = store.pane(pane);
    state.board.set(Some(ActiveEdit { store_id, node_id }));
    ui.layer.set(None);
    let _ = first;
    request_reconcile(store, pane);
    after_change(store, pane);
    crate::pictures::retry_missing();
}

/// Stop showing pane `pane`'s board, if it shows one: the node cache gets the
/// document as this pane last had it.
pub(crate) fn stop(store: AppStore, pane: PaneId) {
    tools::end_text(store, pane);
    // A picture not placed yet goes; a paint selection's pixels stay where
    // they were put.
    let _ = paint_select::drop_placing(store, pane);
    paint_select::commit(store, pane);
    remember_camera(pane, scale_of(pane));
    // A paint stroke under way ends here, and what it held goes out.
    let painting = with_session(pane, |s| match std::mem::replace(&mut s.gesture, Gesture::Idle) {
        Gesture::Paint(stroke) => Some(stroke),
        other => {
            s.gesture = other;
            None
        }
    })
    .flatten();
    if let Some(stroke) = painting {
        let last = stroke.last;
        paint::released(store, pane, stroke, last);
    }
    paint::flush(store, pane, true);
    paint::forget_thumbs(store, pane);
    let Some(session) = SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()].take()) else { return };
    let state = store.pane(pane);
    if session.markup.is_some() {
        // The note's own editor holds the note: the board view over it goes,
        // and the text shows again. The pane's copy (which may hold a board
        // made for a mark that never came) is dropped.
        state.markup.set(false);
        state.show_board.set(false);
    } else if let Some(sig) = store.get_node_signal(session.store_id, session.node_id) {
        let bytes = session.doc.save();
        sig.update(|node| node.content = bytes);
    }
    state.board.set(None);
    state.board_marks.set((0, true));
    let ui = state.board_ui;
    ui.outlines.set(Vec::new());
    ui.handles.set(None);
    ui.selected.set(0);
    ui.marquee.set(None);
    ui.tip.set(None);
    ui.layers.set(Vec::new());
    ui.frames.set(Vec::new());
    ui.others.set_if_changed(Default::default());
}

/// Ask the server for what it has beyond pane `pane`'s copy of its board.
pub(crate) fn request_reconcile(store: AppStore, pane: PaneId) {
    if let Some((store_id, node_id, state_vector)) = with_session(pane, |s| (s.store_id, s.node_id, s.doc.state_vector())) {
        crate::doc_session::ask(store, store_id, node_id, state_vector);
    }
}

/// Every board pane holding the node asks again (the connection came back).
pub(crate) fn request_reconcile_all(store: AppStore, store_id: StoreId, node_id: NodeId) {
    for pane in store.panes_boarding(store_id, node_id) {
        request_reconcile(store, pane);
    }
}

/// The server's answer to a reconcile: merge it into every board pane
/// holding the node, then send back what the server lacks.
pub(crate) fn apply_reconcile(store: AppStore, store_id: StoreId, node_id: NodeId, diff: &[u8], server_state_vector: &[u8]) {
    let writable = crate::doc_session::writable(store, store_id, node_id);
    for pane in store.panes_boarding(store_id, node_id) {
        let ours = with_session(pane, |s| {
            if !diff.is_empty() {
                if let Err(e) = s.doc.apply_update(diff) {
                    tracing::warn!("a board's reconcile did not apply: {e}");
                }
                s.sync_view();
            }
            if writable {
                s.doc.diff_if_peer_lacks_it(server_state_vector, diff).ok().flatten()
            } else {
                None
            }
        })
        .flatten();
        crate::doc_session::send_back(store, store_id, node_id, ours);
        after_change(store, pane);
    }
}

/// A peer's update of a node: applied to the board of every pane holding it.
pub(crate) fn apply_remote(store: AppStore, store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    for pane in store.panes_boarding(store_id, node_id) {
        apply_to(store, pane, store_id, node_id, bytes);
    }
}

fn apply_to(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    let applied = with_session(pane, |s| {
        if (s.store_id, s.node_id) != (store_id, node_id) {
            return false;
        }
        if let Err(e) = s.doc.apply_update(bytes) {
            tracing::warn!("a board update did not apply: {e}");
        }
        s.sync_view();
        true
    });
    if applied == Some(true) {
        tools::text_changed_elsewhere(store, pane);
        after_change(store, pane);
    }
}

/// Send an update this pane made: to the server through `applyEdit`, and to
/// every other pane of this window holding the board, a turn later (the
/// server relays to other clients, not back to this one).
fn send_update(store: AppStore, from: PaneId, store_id: StoreId, node_id: NodeId, bytes: Vec<u8>) {
    crate::doc_session::send(store, store_id, node_id, bytes.clone());
    // A board shown in text (a picture's markup, an embed) follows.
    crate::text_boards::local_update(store_id, node_id, &bytes);
    let others: Vec<PaneId> = store.panes_boarding(store_id, node_id).into_iter().filter(|p| *p != from).collect();
    if others.is_empty() {
        return;
    }
    let deliver = move || {
        for other in others {
            apply_to(store, other, store_id, node_id, &bytes);
        }
    };
    #[cfg(test)]
    deliver();
    #[cfg(not(test))]
    let _ = set_timeout(0, deliver);
}

/// Send an update of node `node_id` made outside any pane's board view (the
/// markup toggle over a picture in text or an embed, a board copied in with a
/// paste): to the server, to every board pane holding the node, and to what
/// shows it in text.
pub(crate) fn send_node_update(store: AppStore, store_id: StoreId, node_id: NodeId, bytes: Vec<u8>) {
    crate::doc_session::send(store, store_id, node_id, bytes.clone());
    crate::text_boards::local_update(store_id, node_id, &bytes);
    for pane in store.panes_boarding(store_id, node_id) {
        apply_to(store, pane, store_id, node_id, &bytes);
    }
}

/// The first mark on a picture in text: the board made when the view
/// opened goes out first, then the atom's `board` is written, so the text
/// names a board that exists.
fn first_mark(store: AppStore, pane: PaneId, held: (StoreId, NodeId)) {
    let Some((pending, attr)) = with_session(pane, |s| {
        let markup = s.markup.as_mut()?;
        let pending = markup.pending.take();
        let attr = std::mem::take(&mut markup.attr_needed).then(|| (markup.src.clone(), markup.pos, markup.board_id.clone()));
        Some((pending, attr))
    })
    .flatten() else {
        return;
    };
    if let Some(bytes) = pending {
        send_update(store, pane, held.0, held.1, bytes);
    }
    if let Some((src, pos, board)) = attr {
        if !crate::text_boards::write_board_attr(pane, &src, pos, &board) {
            tracing::warn!("the picture {src} was not in the text any more; its board {board} stays in the note's data");
        }
    }
}

/// "Mark Up..." on a picture in pane `pane`'s text (contract section 2):
/// the board view over the pane, on the picture's board in the note's
/// `data["board:<id>"]`. A picture with no board yet (one put in before every
/// picture was a canvas, docs/CANVAS_CONTRACT.md) gets one in this pane's copy
/// of the note, shown at once and written with the first mark. An empty canvas
/// in text (`src` [`crate::text_boards::IN_TEXT_SRC`]) always has its board.
pub(crate) fn start_markup(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, src: &str, pos: usize, board: Option<String>) {
    let blob = BlobUrl::parse(src);
    if blob.is_none() && (src != crate::text_boards::IN_TEXT_SRC || board.is_none()) {
        return;
    }
    if !store.node_access(store_id, node_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    stop(store, pane);
    APP.with(|app| app.set(Some(store)));
    let bytes = crate::text_boards::host_bytes(store_id, node_id)
        .or_else(|| crate::editor::editor(pane).collab_snapshot())
        .unwrap_or_default();
    let doc = NodeDoc::load(&bytes).unwrap_or_else(|_| NodeDoc::new());
    let attr_needed = board.is_none();
    let board_id = board.unwrap_or_else(pimble_board::doc::new_id);
    let board = Board::in_text(&doc, &board_id).with_origin(format!("pimble-board-pane-{}", pane.index()));
    if blob.is_none() && !board.exists() {
        crate::events::show_notice(store, "This canvas has not arrived yet.".to_string());
        return;
    }
    let pending = match &blob {
        Some(blob) if !board.exists() => {
            let size = crate::pictures::held_bytes(blob)
                .and_then(|bytes| Picture::decode(&bytes).ok())
                .map(|p| (p.width(), p.height()))
                .unwrap_or((640, 480));
            build_board(&Board::in_text(&doc, &board_id), author().as_ref(), Some((blob, size.0, size.1)))
        }
        _ => None,
    };
    let dark = untracked(|| store.dark_mode.get());
    let ui = store.pane(pane).board_ui;
    let mut view = BoardView::new(1, 1, background(dark));
    view.set_paint_cache(paint::shared_cache());
    if untracked(|| store.board_style.grid.get()) {
        view.set_grid(Some(GRID));
    }
    let mut session = Session {
        store_id,
        node_id,
        doc,
        board,
        watch: None,
        undo: None,
        view,
        fitted: false,
        restore: None,
        gesture: Gesture::Idle,
        selected: Vec::new(),
        space: false,
        shift: false,
        settle_generation: 0,
        last_press: (0.0, 0.0, 0.0),
        pointer: (0.0, 0.0),
        over: false,
        driver: None,
        touches: Vec::new(),
        pressure: 0.5,
        text: None,
        options: ViewOptions { color_by_author: untracked(|| ui.by_author.get()), ..ViewOptions::default() },
        nav_generation: 0,
        tool_chosen: false,
        markup: Some(Markup { src: src.to_string(), pos, board_id, pending, attr_needed }),
        alt: false,
        pen: None,
        points: None,
        clock: Arc::default(),
        outbox: Vec::new(),
        outbox_at: 0.0,
        version: 0,
        paint_pressure: 1.0,
        paint_sel: None,
        ants: 0.0,
    };
    // The watch and the undo manager start after the board was made here,
    // so undo never takes back the board itself.
    session.sync_view();
    SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()] = Some(session));
    let state = store.pane(pane);
    state.board.set(Some(ActiveEdit { store_id, node_id }));
    state.markup.set(true);
    state.show_board.set(true);
    ui.layer.set(None);
    after_change(store, pane);
    crate::pictures::retry_missing();
    // The keyboard goes to the board (its tools' letters, Escape).
    crate::editor::editor(pane).blur();
}

/// A mark on pane `pane`'s board, as a tool would make one (tests).
#[cfg(test)]
pub(crate) fn mark_for_test(store: AppStore, pane: PaneId) {
    local_edit(store, pane, |t| t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(10.0, 10.0, 20.0, 20.0)))));
}

/// A highlighter stroke from board point `from` to `to` on pane `pane`'s
/// board, by mouse events (tests).
#[cfg(test)]
pub(crate) fn highlight_for_test(store: AppStore, pane: PaneId, from: Point, to: Point) {
    use rinch::render_surface::SurfacePointer;
    store.board_tool.set(Tool::Highlighter);
    let screen = |p: Point| with_session(pane, |s| s.view.board_to_screen(p)).unwrap_or_default();
    let (a, b) = (screen(from), screen(to));
    tools::pointer_down(store, pane, a.0, a.1, SurfaceMouseButton::Left, SurfacePointer::mouse(true));
    for i in 1..=4 {
        let k = i as f32 / 4.0;
        tools::pointer_move(store, pane, a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k, SurfacePointer::mouse(true));
    }
    tools::pointer_up(store, pane, b.0, b.1, SurfaceMouseButton::Left, SurfacePointer::mouse(false));
}

/// "Done" on the board view over pane `pane`'s text: back to the text.
pub(crate) fn end_markup(store: AppStore, pane: PaneId) {
    if with_session(pane, |s| s.markup.is_some()) != Some(true) {
        return;
    }
    stop(store, pane);
    crate::toolbar::bump_toolbar(pane);
    crate::editor::editor(pane).focus();
}

/// The decoded pictures `snapshot` names, for drawing it elsewhere (a
/// picture in text, an embed), and whether every one is here; the missing
/// ones are fetched, and `text_boards::picture_arrived` hears when they are.
pub(crate) fn pictures_of(snapshot: &BoardSnapshot) -> (HashMap<String, Arc<Picture>>, bool) {
    let source = picture_source();
    let mut found = HashMap::new();
    let mut all = true;
    // Pictures, and the pictures paint layers' `pixels` operations draw.
    for src in snapshot.picture_srcs() {
        match source(&src) {
            Some(picture) => {
                found.insert(src, picture);
            }
            None => all = false,
        }
    }
    if !all {
        request_wanted_pictures();
    }
    (found, all)
}

/// Whether this device may write pane `pane`'s board; says the read-only
/// sentence when it may not.
fn writable_or_say(store: AppStore, pane: PaneId) -> bool {
    let Some(held) = with_session(pane, |s| (s.store_id, s.node_id)) else { return false };
    if store.node_access(held.0, held.1).allows_write() {
        return true;
    }
    crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
    false
}

/// One local edit of pane `pane`'s board, stamped with who and when: refused
/// (with the sentence) when this device may only read the node. Answers the
/// closure's result.
fn local_edit<R>(store: AppStore, pane: PaneId, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> pimble_board::doc::Result<R>) -> Option<R> {
    local_edit_saying(store, pane, false, f)
}

/// [`local_edit`] that says every refusal (combining, flipping, SVG in:
/// `pimble-board`'s refusals are sentences), not only a lock's.
fn local_edit_said<R>(store: AppStore, pane: PaneId, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> pimble_board::doc::Result<R>) -> Option<R> {
    local_edit_saying(store, pane, true, f)
}

fn local_edit_saying<R>(store: AppStore, pane: PaneId, say: bool, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> pimble_board::doc::Result<R>) -> Option<R> {
    let held = with_session(pane, |s| (s.store_id, s.node_id))?;
    if !writable_or_say(store, pane) {
        return None;
    }
    first_mark(store, pane, held);
    let author = crate::shares::author_in(store, held.0, held.1);
    let now = now_stamp();
    let (result, update) = with_session(pane, |s| {
        let edited = s.board.edit(Stamp::new(author.as_ref(), &now), f);
        s.sync_view();
        (edited.result, edited.update)
    })?;
    if let Some(bytes) = update {
        send_update(store, pane, held.0, held.1, bytes);
    }
    after_change(store, pane);
    match result {
        Ok(r) => Some(r),
        // The tools check locks before they write; a lock that arrived from
        // someone else in between is said the way the board says it.
        Err(e) if say || e.is_lock() => {
            crate::events::show_notice(store, e.to_string());
            None
        }
        Err(e) => {
            tracing::info!("a board edit was refused: {e}");
            None
        }
    }
}

thread_local! {
    /// Per pane: the note last typed in the "Meaning" section, and for what,
    /// so the box is not given back what it just sent.
    static NOTE_TYPED: RefCell<[Option<(MeaningTarget, String)>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
}

/// Publish what the "Meaning" section shows for pane `pane`: the one
/// selected element, or else the picked layer (the top unlocked one when none
/// is picked, as the layers panel picks).
pub(crate) fn publish_meaning(pane: PaneId, ui: PaneUi) {
    let picked = untracked(|| ui.layer.get());
    let view = with_session(pane, |s| {
        let snapshot = s.snapshot();
        if !snapshot.exists {
            return None;
        }
        if let [id] = s.selected.as_slice() {
            let e = snapshot.get(id)?;
            let label = tools::kind_label(e);
            let heading = match e.raw.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                Some(text) => {
                    let short: String = text.chars().take(32).collect();
                    let more = if text.chars().count() > 32 { "…" } else { "" };
                    format!("{label}: {}{more}", short.replace('\n', " "))
                }
                None => label,
            };
            return Some(MeaningView {
                target: MeaningTarget::Element(id.clone()),
                heading,
                note: e.raw.note.clone().unwrap_or_default(),
                fields: e.raw.fields.clone(),
            });
        }
        let layer = match &picked {
            Some(id) => snapshot.layer(id),
            None => snapshot.layer_for_new(None).ok(),
        }?;
        Some(MeaningView {
            target: MeaningTarget::Layer(layer.id.clone()),
            heading: format!("Layer: {}", layer.name),
            note: layer.note.clone().unwrap_or_default(),
            fields: layer.fields.clone(),
        })
    })
    .flatten();
    let before = untracked(|| ui.meaning.get());
    if let Some(v) = &view {
        let typed = NOTE_TYPED.with(|t| t.borrow()[pane.index()].clone());
        let same_target = before.as_ref().is_some_and(|b| b.target == v.target);
        let echo = typed.as_ref().is_some_and(|(target, text)| *target == v.target && *text == v.note);
        // A new target, or someone else's edit of this note: the box takes it.
        if !same_target || (!echo && before.as_ref().is_some_and(|b| b.note != v.note)) {
            let count = untracked(|| ui.note_value.get().0) + 1;
            ui.note_value.set((count, v.note.clone()));
            if !same_target {
                NOTE_TYPED.with(|t| t.borrow_mut()[pane.index()] = None);
                ui.field_edit.set((String::new(), String::new()));
            }
        }
    }
    ui.meaning.set_if_changed(view);
}

/// The note box was typed in: write it to its target.
pub(crate) fn write_note(store: AppStore, pane: PaneId, note: String) {
    let ui = store.pane(pane).board_ui;
    let Some(target) = untracked(|| ui.meaning.get()).map(|m| m.target) else { return };
    // One undo step per stretch of typing in one note.
    let first = NOTE_TYPED.with(|t| t.borrow()[pane.index()].as_ref().map(|(tg, _)| tg != &target).unwrap_or(true));
    if first {
        new_undo_step(pane);
    }
    NOTE_TYPED.with(|t| t.borrow_mut()[pane.index()] = Some((target.clone(), note.clone())));
    local_edit(store, pane, |t| match &target {
        MeaningTarget::Element(id) => t.set_note(id, &note),
        MeaningTarget::Layer(id) => t.set_layer_note(id, &note),
    });
}

/// Set (or with `None` remove) a field of the "Meaning" section's target.
pub(crate) fn write_field(store: AppStore, pane: PaneId, name: &str, value: Option<&str>) {
    let ui = store.pane(pane).board_ui;
    let Some(target) = untracked(|| ui.meaning.get()).map(|m| m.target) else { return };
    if name.trim().is_empty() {
        return;
    }
    new_undo_step(pane);
    local_edit(store, pane, |t| match &target {
        MeaningTarget::Element(id) => t.set_field(id, name, value),
        MeaningTarget::Layer(id) => t.set_layer_field(id, name, value),
    });
}

/// Start a new undo step: the next edit is taken back on its own, however
/// soon after the last.
fn new_undo_step(pane: PaneId) {
    with_session(pane, |s| {
        if let Some(undo) = s.undo.as_mut() {
            undo.reset();
        }
    });
}

/// After the pane's board or view changed: what floats over the board and the
/// panels follow, then the surface is handed a frame if one is due.
fn after_change(store: AppStore, pane: PaneId) {
    let scale = scale_of(pane);
    remember_camera(pane, scale);
    let state = store.pane(pane);
    let ui = state.board_ui;
    // Set after the session is let go of: a signal set while it is held
    // runs effects that may want it.
    let mut pins_out: Option<Vec<PinView>> = None;
    let published = with_session(pane, |s| {
        let snapshot = s.snapshot();
        let marks = (snapshot.markup_count(), snapshot.markup_visible);
        let logical = |r: (f32, f32, f32, f32)| (r.0 / scale, r.1 / scale, r.2 / scale, r.3 / scale);
        // Direct select shows the points of what it edits, not its box.
        let pointing: Option<Vec<String>> = s.points.as_ref().map(|p| p.ids().cloned().collect());
        let outlines: Vec<(f32, f32, f32, f32)> = s
            .selected
            .iter()
            .filter(|id| !pointing.as_ref().is_some_and(|ids| ids.contains(*id)))
            .filter_map(|id| snapshot.get(id))
            .map(|e| logical(s.to_screen(&e.bounds)))
            .collect();
        let handles = if let Some(b) = s.paint_sel.as_ref().and_then(|sel| sel.box_now()) {
            // A paint selection's handles, on its pixels where they are now.
            Some(logical(s.to_screen(&b)))
        } else if pointing.is_none() && s.selected.iter().any(|id| s.editable(id)) {
            s.selection_bounds().map(|b| logical(s.to_screen(&b)))
        } else {
            None
        };
        let layers = layers::rows_of(snapshot);
        let kinds: Vec<Kind> = s.selected.iter().filter_map(|id| snapshot.get(id)).map(|e| e.raw.kind.clone()).collect();
        let zoom = (s.view.zoom() / scale * 100.0).round();
        let frames: Vec<(String, String)> = snapshot
            .frames()
            .into_iter()
            .map(|e| (e.raw.id.clone(), e.raw.text.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "Frame".to_string())))
            .collect();
        let picture = s.selected.len() == 1 && snapshot.get(&s.selected[0]).is_some_and(|e| e.raw.kind.is_picture());
        let authors = snapshot.authors().len();
        let marquee = match &s.gesture {
            Gesture::Marquee { start, now, .. } | Gesture::PointMarquee { start, now, .. } => {
                let (x0, y0) = (start.0.min(now.0), start.1.min(now.1));
                Some(logical((x0, y0, (start.0 - now.0).abs(), (start.1 - now.1).abs())))
            }
            _ => None,
        };
        // Pins (a board node's own board only; markup on a picture in text
        // has none yet).
        let pins: Vec<PinView> = if untracked(|| state.markup.get()) {
            Vec::new()
        } else {
            crate::discussions::board_pins(store, s.store_id, s.node_id)
                .into_iter()
                .enumerate()
                .map(|(i, pin)| {
                    let (x, y) = s.view.board_to_screen(Point::new(pin.x, pin.y));
                    PinView { store_id: s.store_id, node_id: pin.node_id, x: x / scale, y: y / scale, number: i + 1, title: pin.title, count: pin.count, unread: pin.unread }
                })
                .collect()
        };
        pins_out = Some(pins);
        (marks, outlines, handles, layers, frames, picture, authors, marquee, s.selected.len(), kinds, zoom)
    });
    if let Some(pins) = pins_out {
        ui.pins.set(pins);
    }
    // An image opens on the arrow, a whiteboard on select (contract section
    // 4), once its board is here.
    let opening = with_session(pane, |s| {
        if s.tool_chosen || !s.snapshot().exists {
            return None;
        }
        s.tool_chosen = true;
        // An image's picture is in a locked layer of its own; pictures put on
        // a whiteboard are not.
        let snapshot = s.snapshot();
        Some(snapshot.elements.iter().any(|e| e.raw.kind.is_picture() && snapshot.in_locked_layer(e)))
    })
    .flatten();
    if let Some(picture) = opening {
        if untracked(|| store.focused_pane.get()) == pane {
            store.board_tool.set(if picture { Tool::Arrow } else { Tool::Select });
        }
    }
    match published {
        Some((marks, outlines, handles, layers, frames, picture, authors, marquee, count, kinds, zoom)) => {
            state.board_marks.set_if_changed(marks);
            ui.outlines.set_if_changed(outlines);
            ui.handles.set_if_changed(handles);
            ui.selected.set_if_changed(count);
            ui.selected_kinds.set_if_changed(kinds);
            ui.zoom.set_if_changed(zoom);
            // The selected layer must still exist.
            let chosen = untracked(|| ui.layer.get());
            if chosen.as_ref().is_some_and(|id| !layers.iter().any(|l| &l.id == id)) {
                ui.layer.set(None);
            }
            let extra = untracked(|| ui.picked_layers.get());
            if extra.iter().any(|id| !layers.iter().any(|l| &l.id == id)) {
                ui.picked_layers.set(extra.into_iter().filter(|id| layers.iter().any(|l| &l.id == id)).collect());
            }
            ui.layers.set_if_changed(layers);
            publish_meaning(pane, ui);
            ui.frames.set_if_changed(frames);
            ui.picture_selected.set_if_changed(picture);
            ui.authors.set_if_changed(authors);
            ui.marquee.set_if_changed(marquee);
            tools::place_text_box(store, pane);
            ui::refresh_card_labels(store, pane);
        }
        None => {
            ui.outlines.set_if_changed(Vec::new());
            ui.handles.set_if_changed(None);
        }
    }
    points::refresh_overlay(store, pane);
    present(pane);
    nav::changed(store, pane);
    paint::thumbs_due(store, pane);
    draw_others(store, pane);
    crate::presence::moved(store, pane);
}

/// Where pane `pane` is on its board, as its presence frame says it: `None`
/// when it shows no board.
pub(crate) fn presence_mine(store: AppStore, pane: PaneId) -> Option<pimble_rpc::BoardPresence> {
    if !untracked(|| store.pane(pane).show_board.get()) {
        return None;
    }
    with_session(pane, |s| {
        let pointer = s.over.then_some(s.pointer);
        let markup = s.markup.as_ref().map(|m| m.board_id.as_str());
        crate::presence::board::whereabouts(&s.view.view(), s.view.size(), pointer, &s.selected, markup)
    })
}

/// Draw everyone else's pointers and selections over pane `pane`'s board
/// with its camera as it is now.
pub(crate) fn draw_others(store: AppStore, pane: PaneId) {
    use crate::presence::board::{on_screen, rect_on_screen, sessions_on, OutlineView, Others, PointerView};
    let scale = scale_of(pane);
    let ui = store.pane(pane).board_ui;
    let shown = untracked(|| store.pane(pane).show_board.get());
    let held = with_session(pane, |s| (s.store_id, s.node_id, s.markup.as_ref().map(|m| m.board_id.clone()))).filter(|_| shown);
    let Some((store_id, node_id, markup)) = held else {
        ui.others.set_if_changed(Others::default());
        return;
    };
    let sessions = sessions_on(store, store_id, node_id, markup.as_deref());
    let others = with_session(pane, |s| {
        let (view, size) = (s.view.view(), s.view.size());
        let snapshot = s.snapshot();
        let mut others = Others::default();
        for (session, board, who, title) in sessions {
            for id in &board.selection {
                let Some(e) = snapshot.get(id) else { continue };
                let (x, y, w, h) = rect_on_screen(&view, scale, &e.bounds);
                others.outlines.push(OutlineView { x, y, w, h, color: who.color.clone() });
            }
            if let Some((x, y)) = board.pointer.and_then(|p| on_screen(&view, size, scale, p)) {
                others.pointers.push(PointerView {
                    session,
                    key: who.key.clone(),
                    style: format!("left: {x:.1}px; top: {y:.1}px; color: {};", who.color),
                    tag_style: format!("background: {};", who.color),
                    name: who.name,
                    title,
                });
            }
        }
        others
    })
    .unwrap_or_default();
    ui.others.set_if_changed(others);
}

/// Remember where pane `pane`'s board node is looked at, on this device
/// (`views.rs`), once it shows at a camera of its own. A picture's markup
/// is not a node of its own and is not remembered.
fn remember_camera(pane: PaneId, scale: f32) {
    let Some((store_id, node_id, camera)) = with_session(pane, |s| {
        (s.fitted && s.markup.is_none() && s.view.size().0 > 0).then(|| {
            let center = s.view.center();
            (s.store_id, s.node_id, crate::views::Camera::new(center.x, center.y, s.view.zoom() / scale))
        })
    })
    .flatten() else {
        return;
    };
    crate::views::set_camera(store_id, node_id, camera);
}

/// Remember the camera of every pane's board (the window is closing).
#[cfg(feature = "native")]
pub(crate) fn remember_all() {
    for pane in PaneId::ALL {
        remember_camera(pane, scale_of(pane));
    }
}

/// Hand pane `pane`'s surface its next frame, if anything changed: the
/// view at the surface's own physical size, fitted to the board the first
/// time there is one to fit.
fn present(pane: PaneId) {
    let Some(surface) = surface_of(pane) else { return };
    let (width, height) = surface.layout_size();
    if width == 0 || height == 0 {
        return;
    }
    let scale = scale_of(pane);
    let writer = surface.writer();
    with_session(pane, |s| {
        s.view.resize(width, height);
        if let Some(camera) = s.restore.take().filter(|_| !s.fitted) {
            // Where this device left it.
            s.view.set_camera(Point::new(camera.x, camera.y), camera.zoom * scale);
            s.fitted = true;
            s.view.settle(&picture_source());
        }
        if !s.fitted && s.view.snapshot().exists {
            match s.view.snapshot().visible_extent() {
                Some(extent) => {
                    s.view.fit(&extent, 24.0 * scale);
                    // Never larger than one board unit to a CSS pixel: a
                    // small picture opens at its own size.
                    if s.view.zoom() > scale {
                        s.view.set_camera(extent.center(), scale);
                    }
                }
                None => s.view.set_camera(Point::new(0.0, 0.0), scale),
            }
            s.fitted = true;
            // The first frame is at the fitted zoom, not a resample.
            s.view.settle(&picture_source());
        }
        if let Some(frame) = s.view.frame(&picture_source()) {
            writer.submit_frame(frame, width, height);
        }
    });
    request_wanted_pictures();
}

/// The camera moved: the floating boxes follow, a frame is due.
fn camera_moved(store: AppStore, pane: PaneId) {
    after_change(store, pane);
}

// ── Pictures on boards ──────────────────────────────────────────────────────

#[derive(Default)]
struct Pictures {
    /// Decoded, by `src`.
    decoded: HashMap<String, Arc<Picture>>,
    /// Their bytes as stored, by `src` (Save Original As..., SVG export).
    raw: HashMap<String, Arc<Vec<u8>>>,
    /// Asked for by a render and not yet fetched.
    wanted: HashSet<String>,
    /// Asked of the backend (or being decoded).
    asked: HashSet<String>,
    /// Answered "not here": asked again by [`retry_missing_pictures`].
    missing: HashSet<String>,
    /// When each decoded picture was last drawn from (ms).
    used: HashMap<String, f64>,
    /// A trim is due ([`trim_pictures`]).
    trim_armed: bool,
}

/// The decoded pictures boards keep (docs/DRAWING_CONTRACT.md "Large
/// canvases"): past this, those not drawn from for [`PICTURE_IDLE_MS`] go
/// (asked for again, from their bytes kept here, when a render needs them).
/// A county-sized layer flattened at full density is about 130 pictures of
/// 16 MB, which every one were held for good before.
const PICTURE_BUDGET: usize = 512 << 20;
const PICTURE_IDLE_MS: f64 = 4000.0;

/// A decoded picture's bytes: its pixels and its smaller levels.
fn picture_bytes_held(picture: &Picture) -> usize {
    picture.width() as usize * picture.height() as usize * 4 * 4 / 3
}

/// Drop the decoded pictures not drawn from lately while the cache is past
/// its budget; checked again a moment later while anything is held that
/// may go then. Never one drawn from in the last few seconds, so a view
/// that needs more than the budget at once keeps what it shows.
fn trim_pictures() {
    let now = crate::text_boards::now_ms();
    let again = PICTURES.with(|p| {
        let mut p = p.borrow_mut();
        p.trim_armed = false;
        let mut total: usize = p.decoded.values().map(|pic| picture_bytes_held(pic)).sum();
        if total <= PICTURE_BUDGET {
            return false;
        }
        let mut by_age: Vec<(f64, String)> = p.decoded.keys().map(|k| (p.used.get(k).copied().unwrap_or(0.0), k.clone())).collect();
        by_age.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut kept_recent = false;
        for (used, src) in by_age {
            if total <= PICTURE_BUDGET / 10 * 8 {
                break;
            }
            if now - used < PICTURE_IDLE_MS {
                kept_recent = true;
                break;
            }
            if let Some(pic) = p.decoded.remove(&src) {
                total = total.saturating_sub(picture_bytes_held(&pic));
            }
            p.used.remove(&src);
            p.asked.remove(&src);
        }
        tracing::debug!("board pictures: {} decoded, {} MB held after a trim", p.decoded.len(), total >> 20);
        kept_recent && total > PICTURE_BUDGET
    });
    if again {
        trim_pictures_soon();
    }
}

/// [`trim_pictures`] in a moment, once.
fn trim_pictures_soon() {
    let arm = PICTURES.with(|p| !std::mem::replace(&mut p.borrow_mut().trim_armed, true));
    if arm {
        let _ = set_timeout(PICTURE_IDLE_MS as u32 + 500, trim_pictures);
    }
}

thread_local! {
    static PICTURES: RefCell<Pictures> = RefCell::new(Pictures::default());
}

/// The renderer's pictures: the decoded ones, and a note of every one it
/// wanted and did not find.
fn picture_source() -> impl Fn(&str) -> Option<Arc<Picture>> {
    |src: &str| {
        PICTURES.with(|p| {
            let mut p = p.borrow_mut();
            match p.decoded.get(src).cloned() {
                Some(picture) => {
                    p.used.insert(src.to_string(), crate::text_boards::now_ms());
                    Some(picture)
                }
                None => {
                    if !p.asked.contains(src) {
                        p.wanted.insert(src.to_string());
                    }
                    None
                }
            }
        })
    }
}

/// A picture this window made (a flattened layer), put in the cache under
/// the URL it was stored at, so it shows at once and is not fetched back.
fn seed_picture(src: &str, picture: Arc<Picture>, bytes: Vec<u8>) {
    PICTURES.with(|p| {
        let mut p = p.borrow_mut();
        p.decoded.insert(src.to_string(), picture);
        p.used.insert(src.to_string(), crate::text_boards::now_ms());
        p.raw.insert(src.to_string(), Arc::new(bytes));
        p.wanted.remove(src);
    });
}

/// A picture's bytes as stored, when this window has them.
fn picture_bytes(src: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = PICTURES.with(|p| p.borrow().raw.get(src).cloned()) {
        return Some(bytes.as_ref().clone());
    }
    let url = BlobUrl::parse(src)?;
    crate::pictures::held_bytes(&url).map(|b| b.as_ref().clone())
}

/// Fetch what the last frames wanted: from the text pictures' cache when it
/// has the bytes, else from the backend.
fn request_wanted_pictures() {
    let wanted: Vec<String> = PICTURES.with(|p| {
        let mut p = p.borrow_mut();
        let wanted: Vec<String> = p.wanted.drain().collect();
        p.asked.extend(wanted.iter().cloned());
        wanted
    });
    for src in wanted {
        let Some(url) = BlobUrl::parse(&src) else { continue };
        match crate::pictures::held_bytes(&url) {
            Some(bytes) => decode(src, bytes),
            None => {
                if let Some(store) = APP.with(Cell::get) {
                    store.send(BackendCommand::GetBlob { url });
                }
            }
        }
    }
}

/// Ask again for the pictures boards wanted that were not here: one that
/// arrives is decoded and drawn as if it had been there all along.
pub(crate) fn retry_missing_pictures() {
    let any = PICTURES.with(|p| {
        let mut p = p.borrow_mut();
        let missing: Vec<String> = p.missing.drain().collect();
        let any = !missing.is_empty();
        p.wanted.extend(missing);
        any
    });
    if any {
        request_wanted_pictures();
    }
}

/// The answer to a `GetBlob`: a board waiting for that picture gets it.
pub(crate) fn blob_loaded(url: &BlobUrl, result: &Result<Arc<Vec<u8>>, String>) {
    let src = url.to_string();
    if !PICTURES.with(|p| p.borrow().asked.contains(&src)) {
        return;
    }
    match result {
        Ok(bytes) => decode(src, bytes.clone()),
        // Not here (yet): asked again shortly (`pictures::ask_again_later`).
        Err(_) => {
            PICTURES.with(|p| {
                let mut p = p.borrow_mut();
                p.asked.remove(&src);
                p.missing.insert(src);
            });
            crate::pictures::ask_again_later();
        }
    }
}

/// Decode a picture's bytes, off the UI thread on the desktop, and show it on
/// every board that names it.
fn decode(src: String, bytes: Arc<Vec<u8>>) {
    PICTURES.with(|p| {
        p.borrow_mut().raw.insert(src.clone(), bytes.clone());
    });
    #[cfg(feature = "native")]
    std::thread::spawn(move || {
        let picture = Picture::decode(&bytes);
        run_on_main_thread(move || installed(src, picture.map(Arc::new).map_err(|e| e.to_string())));
    });
    #[cfg(not(feature = "native"))]
    {
        let _ = set_timeout(0, move || installed(src, Picture::decode(&bytes).map(Arc::new).map_err(|e| e.to_string())));
    }
}

fn installed(src: String, picture: Result<Arc<Picture>, String>) {
    match picture {
        Ok(picture) => {
            PICTURES.with(|p| {
                let mut p = p.borrow_mut();
                p.decoded.insert(src.clone(), picture);
                p.used.insert(src.clone(), crate::text_boards::now_ms());
            });
            trim_pictures_soon();
            crate::text_boards::picture_arrived();
        }
        Err(e) => {
            tracing::warn!("the picture {src} could not be decoded: {e}");
            return;
        }
    }
    for pane in PaneId::ALL {
        let touched = with_session(pane, |s| {
            let mut rects: Vec<Rect> = s
                .view
                .snapshot()
                .elements
                .iter()
                .filter(|e| matches!(&e.raw.geom, Geom::Picture(g) if g.src == src))
                .map(|e| e.bounds)
                .collect();
            rects.extend(paint::pixels_bounds(s.view.snapshot(), &src));
            for rect in &rects {
                s.view.invalidate(*rect);
            }
            s.nav_generation += 1;
            !rects.is_empty()
        });
        if touched == Some(true) {
            present(pane);
            if let Some(store) = APP.with(Cell::get) {
                nav::changed(store, pane);
            }
        }
    }
}

// ── The view's own settings ─────────────────────────────────────────────────

/// Fit pane `pane`'s board to what it shows.
pub(crate) fn fit(store: AppStore, pane: PaneId) {
    with_session(pane, |s| {
        s.fitted = false;
        s.restore = None;
    });
    after_change(store, pane);
}

/// Show pane `pane`'s board at one board unit to a CSS pixel.
pub(crate) fn actual_size(store: AppStore, pane: PaneId) {
    let scale = scale_of(pane);
    with_session(pane, |s| {
        let center = s.view.center();
        s.view.set_camera(center, scale);
        s.view.settle(&picture_source());
    });
    after_change(store, pane);
}

/// Show board rectangle `rect` in pane `pane` (a frame from the navigator).
pub(crate) fn show_rect(store: AppStore, pane: PaneId, rect: Rect) {
    let scale = scale_of(pane);
    with_session(pane, |s| {
        s.view.fit(&rect, 32.0 * scale);
        s.view.settle(&picture_source());
    });
    after_change(store, pane);
}

/// Show what board rectangle `viewport` covers in pane `pane`, as nearly as
/// its own shape allows (following someone, docs/PRESENCE_CONTRACT.md
/// section 7). Answers false when the pane has no board session yet.
pub(crate) fn take_viewport(store: AppStore, pane: PaneId, viewport: &pimble_rpc::BoardRect) -> bool {
    if viewport.w <= 0.0 || viewport.h <= 0.0 {
        return true;
    }
    let rect = Rect::new(viewport.x, viewport.y, viewport.w, viewport.h);
    let taken = with_session(pane, |s| {
        if s.view.size().0 == 0 {
            return false;
        }
        s.fitted = true;
        s.view.fit(&rect, 0.0);
        s.view.settle(&picture_source());
        true
    });
    if taken != Some(true) {
        return false;
    }
    after_change(store, pane);
    true
}

/// The floating markup control (contract section 5): show or hide every
/// mark on the board, for everyone.
pub(crate) fn toggle_markup(store: AppStore, pane: PaneId) {
    let Some(visible) = with_session(pane, |s| s.view.snapshot().markup_visible) else { return };
    new_undo_step(pane);
    local_edit(store, pane, |t| t.set_markup_visible(!visible));
}

/// The theme changed: every board's background follows.
pub(crate) fn set_dark_mode(dark: bool) {
    for pane in PaneId::ALL {
        if with_session(pane, |s| s.view.set_background(background(dark))).is_some() {
            present(pane);
        }
    }
}

/// The grid shown or hidden (per viewer): every board follows.
pub(crate) fn set_grid(store: AppStore, shown: bool) {
    for pane in PaneId::ALL {
        if with_session(pane, |s| s.view.set_grid(shown.then_some(GRID))).is_some() {
            after_change(store, pane);
        }
    }
}

/// "Color by author" in pane `pane` (contract section 6): a way of looking
/// that hides nothing, this pane only.
pub(crate) fn set_color_by_author(store: AppStore, pane: PaneId, on: bool) {
    store.pane(pane).board_ui.by_author.set(on);
    let changed = with_session(pane, |s| {
        if s.options.color_by_author == on {
            return false;
        }
        s.options.color_by_author = on;
        let options = s.options.clone();
        s.view.set_options(options, &s.board);
        true
    });
    if changed == Some(true) {
        after_change(store, pane);
    }
}

/// Node cards' live titles and colors (contract section 4): the view is
/// re-read only when they changed.
fn set_card_labels(store: AppStore, pane: PaneId, labels: HashMap<String, CardLabel>) {
    let changed = with_session(pane, |s| {
        if *s.options.card_labels == labels {
            return false;
        }
        s.options.card_labels = Arc::new(labels);
        let options = s.options.clone();
        s.view.set_options(options, &s.board);
        true
    });
    if changed == Some(true) {
        present(pane);
        let _ = store;
    }
}

/// Pane `pane`'s surface, made once and wired to its board.
pub(crate) fn surface(store: AppStore, pane: PaneId) -> RenderSurfaceHandle {
    if let Some(surface) = surface_of(pane) {
        return surface;
    }
    APP.with(|app| app.set(Some(store)));
    let surface = create_render_surface();
    surface.set_render_callback(move |_writer, _width, _height| present(pane));
    // Pointer events: which device (pen, its eraser, a finger) and how hard,
    // two-finger and Ctrl+wheel zoom as `Pinch`.
    surface.set_pointer_events(true);
    surface.set_event_handler(move |event| tools::on_event(store, pane, event));
    surface.set_key_handler(move |key| tools::claims_key(pane, key));
    SURFACES.with(|s| s.borrow_mut()[pane.index()] = Some(surface.clone()));
    surface
}

// ── Making board nodes ──────────────────────────────────────────────────────

/// The update that makes a new board: one layer to draw in and, for an image
/// node, the picture at one unit per pixel in a locked layer of its own below
/// it (contract sections 1 and 3).
pub(crate) fn new_board(picture: Option<(&BlobUrl, u32, u32)>) -> Option<Vec<u8>> {
    build_board(&Board::standalone(), author().as_ref(), picture)
}

/// Make `board`, empty or starting from a picture, the way every canvas
/// starts (`BoardTxn::start_canvas`, docs/CANVAS_CONTRACT.md). Answers the
/// update that made it.
pub(crate) fn build_board(board: &Board, author: Option<&Author>, picture: Option<(&BlobUrl, u32, u32)>) -> Option<Vec<u8>> {
    let now = now_stamp();
    let src = picture.map(|(url, _, _)| url.to_string());
    let start = picture.zip(src.as_deref()).map(|((_, px_w, px_h), src)| StartingPicture { src, px_w, px_h });
    let edited = board.edit(Stamp::new(author, &now), |t| t.start_canvas(start).map(|_| ()));
    match edited.result {
        Ok(()) => edited.update,
        Err(e) => {
            tracing::warn!("a new board could not be made: {e}");
            None
        }
    }
}

/// Make a board node under `parent_id`, its board written in the edit that
/// creates it (two devices creating one board would keep only one).
/// The node is `pimble_api::recipes::canvas`; `from_picture` picks its icon.
pub(crate) fn create_board_node(store: AppStore, store_id: StoreId, parent_id: NodeId, title: &str, from_picture: bool, board: Vec<u8>, request: Option<u64>) {
    create_board_node_at(store, store_id, parent_id, title, from_picture, board, request, Default::default());
}

#[allow(clippy::too_many_arguments)]
fn create_board_node_at(store: AppStore, store_id: StoreId, parent_id: NodeId, title: &str, from_picture: bool, board: Vec<u8>, request: Option<u64>, place: pimble_core::Place) {
    let (title, init) = NodeInit::of(pimble_api::recipes::canvas(title, board, from_picture), request);
    store.send(BackendCommand::CreateNode { store_id, parent_id: Some(parent_id), title, init: Some(init), place });
}

/// "New Canvas": an empty board under `parent_id`, at `place`.
pub(crate) fn new_whiteboard(store: AppStore, store_id: StoreId, parent_id: NodeId, place: pimble_core::Place) {
    if let Some(board) = new_board(None) {
        create_board_node_at(store, store_id, parent_id, "Canvas", false, board, None, place);
    }
}

/// Where pictures that are being added go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Dest {
    /// A canvas node per picture under a node: dropped on a tree row, "New
    /// Canvas from Picture...", a paste with a row selected.
    Under(StoreId, NodeId),
    /// On pane `pane`'s board (contract section 1: "Pictures go on any
    /// board"): at a point over its surface (physical pixels), or in the
    /// middle of the view.
    Board(PaneId, Option<(f32, f32)>),
}

/// A picture on its way to becoming an image node, or onto a board.
struct QueuedImage {
    store_id: StoreId,
    /// The node the picture is stored as a write of: the image node's parent,
    /// or the board's own node.
    node_id: NodeId,
    title: String,
    bytes: Vec<u8>,
    /// The board batch it belongs to, for a picture going onto a board.
    batch: Option<u64>,
}

/// Pictures going onto one board together (one paste, drop or pick): placed
/// in one edit once the last of them is stored, side by side in order.
struct BoardBatch {
    pane: PaneId,
    /// The board's node when the pictures were added; a pane that holds
    /// another by then gets none of them.
    held: (StoreId, NodeId),
    at: Option<(f32, f32)>,
    /// What came back so far, in order.
    stored: Vec<(BlobUrl, u32, u32)>,
    remaining: usize,
}

thread_local! {
    /// Pictures to store, in order: one at a time, so image nodes appear and
    /// pictures line up in the order the files came.
    static IMAGE_QUEUE: RefCell<VecDeque<QueuedImage>> = const { RefCell::new(VecDeque::new()) };
    /// The picture being stored now: its request id, where it goes, its title.
    static IN_FLIGHT: RefCell<Option<(u64, StoreId, NodeId, String, Option<u64>)>> = const { RefCell::new(None) };
    static NEXT_REQUEST: Cell<u64> = const { Cell::new(REQUEST_BASE) };
    static BATCHES: RefCell<HashMap<u64, BoardBatch>> = RefCell::new(HashMap::new());
    static NEXT_BATCH: Cell<u64> = const { Cell::new(0) };
}

/// The title an image node takes: its file's name without the extension, or
/// "Picture" for a pasted bitmap.
fn image_title(name: Option<&str>) -> String {
    let Some(name) = name.map(|n| n.rsplit(['/', '\\']).next().unwrap_or(n)) else { return "Picture".to_string() };
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    };
    if stem.is_empty() {
        "Picture".to_string()
    } else {
        stem.to_string()
    }
}

/// Add pictures, in order, where `dest` says. Each picture is fitted and
/// stored as text pictures are (`PutBlob`: scaled to fit, refused with a
/// sentence), then becomes a `picture` element at one unit per pixel: of a
/// new image node's board, or of the board in a pane.
pub(crate) fn add_pictures(store: AppStore, dest: Dest, pictures: Vec<(Option<String>, Vec<u8>)>) {
    match dest {
        Dest::Under(store_id, parent_id) => add_image_nodes(store, store_id, parent_id, pictures),
        Dest::Board(pane, at) => add_to_board(store, pane, at, pictures),
    }
}

/// Make an image node per picture under `parent_id`, in order: dropped
/// files, "New Canvas from Picture...", a paste on a tree row.
pub(crate) fn add_image_nodes(store: AppStore, store_id: StoreId, parent_id: NodeId, pictures: Vec<(Option<String>, Vec<u8>)>) {
    if !store.node_access(store_id, parent_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    IMAGE_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        for (name, bytes) in pictures {
            q.push_back(QueuedImage { store_id, node_id: parent_id, title: image_title(name.as_deref()), bytes, batch: None });
        }
    });
    pump(store);
}

/// Put pictures on pane `pane`'s board (a paste, files dropped on it, the
/// toolbar's picture button): refused on a board this device may only read
/// and when every layer is locked, before anything is stored.
pub(crate) fn add_to_board(store: AppStore, pane: PaneId, at: Option<(f32, f32)>, pictures: Vec<(Option<String>, Vec<u8>)>) {
    let Some(held) = with_session(pane, |s| (s.store_id, s.node_id)) else { return };
    // Into a paint layer (picked, or a paint tool in hand) a picture is
    // placed as pixels (docs/DRAWING_WAVE6_CONTRACT.md section 5).
    if let Some(layer) = paint_select::paints_into(store, pane) {
        paint_select::place_pictures(store, pane, at, layer, pictures);
        return;
    }
    if pictures.is_empty() || !writable_or_say(store, pane) || tools::target_layer(store, pane).is_none() {
        return;
    }
    let batch = NEXT_BATCH.with(|n| n.replace(n.get() + 1));
    BATCHES.with(|b| b.borrow_mut().insert(batch, BoardBatch { pane, held, at, stored: Vec::new(), remaining: pictures.len() }));
    IMAGE_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        for (name, bytes) in pictures {
            q.push_back(QueuedImage { store_id: held.0, node_id: held.1, title: image_title(name.as_deref()), bytes, batch: Some(batch) });
        }
    });
    pump(store);
}

fn pump(store: AppStore) {
    if IN_FLIGHT.with(|f| f.borrow().is_some()) {
        return;
    }
    let Some(next) = IMAGE_QUEUE.with(|q| q.borrow_mut().pop_front()) else { return };
    let request_id = NEXT_REQUEST.with(|n| n.replace(n.get() + 1));
    IN_FLIGHT.with(|f| *f.borrow_mut() = Some((request_id, next.store_id, next.node_id, next.title, next.batch)));
    // The picture is stored as a write of the node it goes under (which is
    // what making a child there is), or of the board it goes on.
    store.send(BackendCommand::PutBlob { store_id: next.store_id, node_id: next.node_id, bytes: next.bytes, request_id });
}

/// The answer to an image node's or a board picture's `PutBlob`; answers
/// whether it was one.
pub(crate) fn stored(store: AppStore, request_id: u64, result: &Result<StoredBlob, String>) -> bool {
    // A flattened layer's picture (Flatten, a merge as pixels).
    if layers::raster_stored(store, request_id, result) {
        return true;
    }
    // A picture placed in a paint layer.
    if paint_select::placed_stored(store, request_id, result) {
        return true;
    }
    let Some((_, store_id, parent_id, title, batch)) = IN_FLIGHT.with(|f| f.borrow_mut().take_if(|(id, ..)| *id == request_id)) else {
        return false;
    };
    let size = match result {
        Ok(blob) => match blob.size {
            Some((w, h)) if w > 0 && h > 0 => {
                if let Some(notice) = &blob.notice {
                    crate::events::show_notice(store, notice.clone());
                }
                Some((blob.url, w, h))
            }
            _ => {
                crate::events::show_notice(store, NOT_A_PICTURE.to_string());
                None
            }
        },
        Err(sentence) => {
            crate::events::show_notice(store, sentence.clone());
            None
        }
    };
    match batch {
        None => {
            if let Some((url, w, h)) = size {
                if let Some(board) = new_board(Some((&url, w, h))) {
                    create_board_node(store, store_id, parent_id, &title, true, board, None);
                }
            }
        }
        Some(batch) => {
            let done = BATCHES.with(|b| {
                let mut b = b.borrow_mut();
                let entry = b.get_mut(&batch)?;
                entry.stored.extend(size);
                entry.remaining = entry.remaining.saturating_sub(1);
                if entry.remaining == 0 { b.remove(&batch) } else { None }
            });
            if let Some(done) = done {
                place_batch(store, done);
            }
        }
    }
    pump(store);
    true
}

/// Lay pictures out side by side, in order, `GAP` apart, centered on a
/// board point, each at one unit per pixel, all scaled down together until
/// they fit inside `view` (a board rectangle). Answers each picture's rectangle.
fn lay_out(sizes: &[(u32, u32)], center: Point, view: Rect) -> Vec<Rect> {
    const GAP: f32 = 16.0;
    /// How much of the view placed pictures may take at most.
    const FILL: f32 = 0.9;
    let total_w = sizes.iter().map(|s| s.0 as f32).sum::<f32>() + GAP * sizes.len().saturating_sub(1) as f32;
    let max_h = sizes.iter().map(|s| s.1 as f32).fold(0.0, f32::max);
    let mut factor = 1.0_f32;
    if total_w > 0.0 && view.w > 0.0 {
        factor = factor.min(view.w * FILL / total_w);
    }
    if max_h > 0.0 && view.h > 0.0 {
        factor = factor.min(view.h * FILL / max_h);
    }
    let mut x = center.x - total_w * factor / 2.0;
    sizes
        .iter()
        .map(|&(w, h)| {
            let (w, h) = (w as f32 * factor, h as f32 * factor);
            let rect = Rect::new(x, center.y - h / 2.0, w, h);
            x += w + GAP * factor;
            rect
        })
        .collect()
}

/// A board batch is all stored: its pictures go on the board in one edit (one
/// undo step), in the layer new elements go to, and are selected.
fn place_batch(store: AppStore, batch: BoardBatch) {
    if batch.stored.is_empty() {
        return;
    }
    let pane = batch.pane;
    if with_session(pane, |s| (s.store_id, s.node_id)) != Some(batch.held) {
        crate::events::show_notice(store, "The canvas was closed before its pictures were ready, so they were not added.".to_string());
        return;
    }
    let Some(layer) = tools::target_layer(store, pane) else { return };
    let Some((center, view)) = with_session(pane, |s| {
        let (w, h) = s.view.size();
        let center = match batch.at {
            Some((x, y)) => s.view.screen_to_board(x, y),
            None => s.view.center(),
        };
        let a = s.view.screen_to_board(0.0, 0.0);
        let b = s.view.screen_to_board(w as f32, h as f32);
        (center, Rect::new(a.x.min(b.x), a.y.min(b.y), (b.x - a.x).abs(), (b.y - a.y).abs()))
    }) else {
        return;
    };
    let sizes: Vec<(u32, u32)> = batch.stored.iter().map(|&(_, w, h)| (w, h)).collect();
    let rects = lay_out(&sizes, center, view);
    new_undo_step(pane);
    let ids = local_edit(store, pane, |t| {
        let mut ids = Vec::new();
        for ((url, w, h), rect) in batch.stored.iter().zip(rects) {
            let geom = PictureGeom { rect, src: url.to_string(), px_w: *w, px_h: *h, crop: None };
            ids.push(t.add_element(NewElement::new(Kind::Picture, Geom::Picture(geom)).in_layer(layer.clone()))?);
        }
        Ok(ids)
    });
    if let Some(ids) = ids {
        // Placed pictures are moved and sized next, so the select tool.
        if untracked(|| store.focused_pane.get()) == pane {
            store.board_tool.set(Tool::Select);
        }
        tools::select(store, pane, ids);
        request_wanted_pictures();
    }
}

/// A row's "New Canvas from Picture..." (an image node of each picture picked), or the
/// tool rail's picture button (each picture onto the board).
pub(crate) fn pick_images(store: AppStore, dest: Dest) {
    #[cfg(feature = "native")]
    {
        // Driving the app over rinch's debug port cannot work a file dialog:
        // this names files to take instead (`:`-separated). Never set in
        // ordinary use.
        let paths: Vec<std::path::PathBuf> = match std::env::var("PIMBLE_DEBUG_IMAGES") {
            Ok(list) => std::env::split_paths(&list).collect(),
            Err(_) => rinch::dialogs::open_file()
                .set_title(if matches!(dest, Dest::Board(..)) { "Insert Pictures" } else { "New Canvas from Picture" })
                .add_filter("Pictures", &["png", "jpg", "jpeg", "gif", "webp"])
                .pick_files()
                .unwrap_or_default(),
        };
        add_image_files(store, dest, paths);
    }
    #[cfg(not(feature = "native"))]
    pick_in_browser(store, dest);
}

/// Files dropped on a tree row or a board, or picked: each picture where
/// `dest` says, in order. Files that are not pictures are left out with a
/// sentence.
#[cfg(feature = "native")]
pub(crate) fn add_image_files(store: AppStore, dest: Dest, paths: Vec<std::path::PathBuf>) {
    let mut pictures = Vec::new();
    let mut svgs = Vec::new();
    let mut skipped = 0;
    for path in paths {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string());
        match std::fs::read(&path) {
            // SVG dropped on a board is drawn in (docs/DRAWING_CONTRACT.md,
            // "SVG in").
            Ok(bytes) if matches!(dest, Dest::Board(..)) && svg_import::is_svg_file(name.as_deref(), &bytes) => svgs.push((name, bytes)),
            Ok(bytes) if pimble_core::ImageMime::sniff(&bytes).is_some() => {
                pictures.push((path.file_name().map(|n| n.to_string_lossy().to_string()), bytes));
            }
            Ok(_) => skipped += 1,
            Err(e) => crate::events::show_notice(store, format!("\"{}\" could not be read: {e}.", path.display())),
        }
    }
    if skipped > 0 {
        crate::events::show_notice(store, NOT_A_PICTURE.to_string());
    }
    if let (Dest::Board(pane, at), false) = (dest, svgs.is_empty()) {
        svg_import::import_files(store, pane, at, svgs);
    }
    if !pictures.is_empty() {
        add_pictures(store, dest, pictures);
    }
}

/// A row's "Paste Picture" on the desktop (the picture on the clipboard becomes
/// an image node under the row), or Ctrl+V on a board (it goes on the board).
#[cfg(feature = "native")]
pub(crate) fn paste_image(store: AppStore, dest: Dest) {
    let image = match rinch::clipboard::paste_image() {
        Ok(image) => image,
        Err(_) => {
            crate::events::show_notice(store, "There is no picture on the clipboard.".to_string());
            return;
        }
    };
    let (w, h) = (image.width as u32, image.height as u32);
    let mut png = Vec::new();
    let encoded = ::image::ImageEncoder::write_image(
        ::image::codecs::png::PngEncoder::new(&mut png),
        &image.bytes,
        w,
        h,
        ::image::ExtendedColorType::Rgba8,
    );
    if encoded.is_err() {
        crate::events::show_notice(store, NOT_A_PICTURE.to_string());
        return;
    }
    add_pictures(store, dest, vec![(None, png)]);
}

/// The browser's picker for "New Canvas from Picture..." and the board's picture button: a
/// hidden file input taking several pictures, read in order.
#[cfg(not(feature = "native"))]
fn pick_in_browser(store: AppStore, dest: Dest) {
    use wasm_bindgen::JsCast;

    const INPUT_ID: &str = "pimble-image-node-input";
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    if let Some(old) = document.get_element_by_id(INPUT_ID) {
        old.remove();
    }
    let Some(input) = document.create_element("input").ok().and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok()) else {
        return;
    };
    input.set_id(INPUT_ID);
    input.set_type("file");
    input.set_multiple(true);
    input.set_accept("image/png,image/jpeg,image/gif,image/webp");
    input.set_hidden(true);
    let Some(body) = document.body() else { return };
    if body.append_child(&input).is_err() {
        return;
    }
    let picked = input.clone();
    // Taken once: a browser may report the change twice, and a one-shot
    // closure called again throws.
    let taken = std::rc::Rc::new(Cell::new(false));
    let on_change = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
        if taken.replace(true) {
            return;
        }
        let files = picked.files();
        picked.remove();
        if let Some(files) = files {
            read_files(store, dest, files, false);
        }
    });
    if input.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref()).is_err() {
        return;
    }
    on_change.forget();
    input.click();
}

/// Read a browser `FileList` in order and put each picture where `dest`
/// says. `pasted`: a clipboard bitmap, which the browser names `image.png`;
/// an image node is called "Picture" instead.
#[cfg(not(feature = "native"))]
fn read_files(store: AppStore, dest: Dest, files: web_sys::FileList, pasted: bool) {
    let mut files: Vec<web_sys::File> = (0..files.length()).filter_map(|i| files.get(i)).collect();
    // SVG put on a board is drawn in (docs/DRAWING_CONTRACT.md, "SVG in").
    if let Dest::Board(pane, at) = dest {
        let (svgs, rest): (Vec<_>, Vec<_>) = files.into_iter().partition(svg_import::is_svg_web_file);
        svg_import::read_files(store, pane, at, svgs);
        files = rest;
    }
    if files.is_empty() {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        let mut pictures = Vec::new();
        for file in files {
            let name = file.name();
            match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => {
                    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                    if pimble_core::ImageMime::sniff(&bytes).is_some() {
                        pictures.push(((!pasted).then_some(name), bytes));
                    } else {
                        crate::events::show_notice(store, NOT_A_PICTURE.to_string());
                    }
                }
                Err(_) => crate::events::show_notice(store, format!("\"{name}\" could not be read.")),
            }
        }
        if !pictures.is_empty() {
            add_pictures(store, dest, pictures);
        }
    });
}

/// The browser's way in for a drop of image files on a tree row or a board,
/// and a paste with a tree row or a board focused: page-level listeners,
/// installed once, that find the row by its `data-pimble-row` attribute
/// (dropped) or the tree's selected row (pasted while no editor or field has
/// the keyboard), and the board by its canvas's `data-pimble-board` (dropped)
/// or its surface having the keyboard (pasted).
#[cfg(not(feature = "native"))]
pub(crate) fn install_browser_handlers(store: AppStore) {
    use wasm_bindgen::JsCast;

    thread_local! {
        static INSTALLED: Cell<bool> = const { Cell::new(false) };
    }
    if INSTALLED.with(|i| i.replace(true)) {
        return;
    }
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };

    fn row_of(target: Option<web_sys::EventTarget>) -> Option<String> {
        let element = target?.dyn_into::<web_sys::Element>().ok()?;
        element.closest("[data-pimble-row]").ok()??.get_attribute("data-pimble-row")
    }
    /// The board pane a drop is over and where, physical pixels over its
    /// surface.
    fn board_of(event: &web_sys::DragEvent) -> Option<(PaneId, (f32, f32))> {
        let element = event.target()?.dyn_into::<web_sys::Element>().ok()?;
        let canvas = element.closest("[data-pimble-board]").ok()??;
        let pane = PaneId::from_index(canvas.get_attribute("data-pimble-board")?.parse().ok()?)?;
        let rect = canvas.get_bounding_client_rect();
        let scale = scale_of(pane);
        let x = (event.client_x() as f64 - rect.left()) as f32 * scale;
        let y = (event.client_y() as f64 - rect.top()) as f32 * scale;
        Some((pane, (x, y)))
    }
    fn carries_files(event: &web_sys::DragEvent) -> bool {
        event.data_transfer().is_some_and(|dt| dt.types().iter().any(|t| t.as_string().as_deref() == Some("Files")))
    }

    let over = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::DragEvent)>::new(move |event: web_sys::DragEvent| {
        if carries_files(&event) && (row_of(event.target()).is_some() || board_of(&event).is_some()) {
            event.prevent_default();
        }
    });
    let _ = document.add_event_listener_with_callback("dragover", over.as_ref().unchecked_ref());
    over.forget();

    let drop = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::DragEvent)>::new(move |event: web_sys::DragEvent| {
        if !carries_files(&event) {
            return;
        }
        let dest = if let Some((pane, at)) = board_of(&event) {
            Dest::Board(pane, Some(at))
        } else {
            let Some(row) = row_of(event.target()) else { return };
            let Some((store_id, parent_id)) = crate::tree_view::create_parent_of_row(store, &row) else { return };
            Dest::Under(store_id, parent_id)
        };
        event.prevent_default();
        let Some(files) = event.data_transfer().and_then(|dt| dt.files()) else { return };
        read_files(store, dest, files, false);
    });
    let _ = document.add_event_listener_with_callback("drop", drop.as_ref().unchecked_ref());
    drop.forget();

    let paste = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::ClipboardEvent)>::new(move |event: web_sys::ClipboardEvent| {
        if event.default_prevented() {
            return;
        }
        // A board with the keyboard (and no text being typed on it) takes
        // the picture.
        if let Some(pane) = keyboard_board() {
            // SVG markup pasted as text is drawn in.
            if let Some(text) = event.clipboard_data().and_then(|dt| dt.get_data("text/plain").ok()).filter(|t| svg_import::is_svg_markup(t)) {
                event.prevent_default();
                svg_import::import_text(store, pane, &text, None);
                return;
            }
            let Some(files) = event.clipboard_data().and_then(|dt| dt.files()) else { return };
            if files.length() == 0 {
                return;
            }
            event.prevent_default();
            read_files(store, Dest::Board(pane, None), files, true);
            return;
        }
        // Otherwise only when nothing that takes text has the keyboard.
        let focused = web_sys::window().and_then(|w| w.document()).and_then(|d| d.active_element());
        if focused.is_some_and(|el| {
            let tag = el.tag_name().to_ascii_lowercase();
            tag == "input" || tag == "textarea" || el.closest("[data-pm-editor]").ok().flatten().is_some() || el.closest("[contenteditable]").ok().flatten().is_some()
        }) {
            return;
        }
        if untracked(|| store.pane(store.focused()).show_editor.get()) {
            return;
        }
        let Some(files) = event.clipboard_data().and_then(|dt| dt.files()) else { return };
        if files.length() == 0 {
            return;
        }
        let Some(row) = untracked(|| store.selected_id.get()) else { return };
        let Some((store_id, parent_id)) = crate::tree_view::create_parent_of_row(store, &row) else { return };
        event.prevent_default();
        read_files(store, Dest::Under(store_id, parent_id), files, true);
    });
    let _ = document.add_event_listener_with_callback("paste", paste.as_ref().unchecked_ref());
    paste.forget();
}

/// The board pane whose surface has the keyboard, unless text is being typed
/// on it.
#[cfg(not(feature = "native"))]
fn keyboard_board() -> Option<PaneId> {
    let focused = rinch::render_surface::focused_surface_id()?;
    PaneId::ALL.into_iter().find(|&pane| {
        surface_of(pane).is_some_and(|s| s.id() == focused) && with_session(pane, |s| s.text.is_none()).unwrap_or(false)
    })
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::protocol::{BackendEvent, BackendHandle};
    use crossbeam_channel::bounded;

    const P: [PaneId; MAX_PANES] = PaneId::ALL;

    fn store_with_commands() -> (AppStore, crossbeam_channel::Receiver<BackendCommand>) {
        let store = AppStore::new();
        let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(256);
        let (_event_tx, event_rx) = bounded::<BackendEvent>(1);
        store.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
        (store, cmd_rx)
    }

    fn whiteboard_bytes() -> Vec<u8> {
        let mut doc = NodeDoc::new();
        doc.init(node_types::BOARD, "Whiteboard", None, "2026-10-09T10:00:00.000Z").unwrap();
        doc.apply_update(&new_board(None).unwrap()).unwrap();
        doc.save()
    }

    fn texts(pane: PaneId) -> usize {
        with_session(pane, |s| s.view.snapshot().elements.len()).unwrap()
    }

    /// The same board in two panes (the editors' rule, docs/SPLIT_VIEW_CONTRACT.md):
    /// an edit in one reaches the other and goes to the server once; a
    /// peer's update reaches both; the node is subscribed once and let go
    /// of when the last pane does.
    #[test]
    fn the_same_board_in_two_panes_stays_in_step() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        let bytes = whiteboard_bytes();
        start(store, P[0], store_id, node_id, &bytes);
        start(store, P[1], store_id, node_id, &bytes);
        // Both panes reconcile; the node's changes come on its store's
        // subscription, never one of its own.
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(sent.iter().filter(|c| matches!(c, BackendCommand::ReconcileNodeContent { .. })).count(), 2);

        let id = local_edit(store, P[0], |t| t.add_element(NewElement::stroke(Kind::Pen, vec![[0.0, 0.0, 0.5], [10.0, 10.0, 0.5]])));
        assert!(id.is_some());
        assert_eq!(texts(P[0]), 1);
        assert_eq!(texts(P[1]), 1, "handed across");
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(sent.iter().filter(|c| matches!(c, BackendCommand::BroadcastChanges { .. })).count(), 1);

        // A peer's stroke arrives as `RemoteChanges` would bring it.
        let mut peer = NodeDoc::load(&bytes).unwrap();
        let before = peer.state_vector();
        let peer_board = Board::of_node(&peer);
        peer_board
            .add_element(Stamp::new(None, "2026-10-09T10:00:01.000Z"), NewElement::stroke(Kind::Pen, vec![[5.0, 5.0, 0.5]]))
            .unwrap();
        let delta = peer.diff_since(&before).unwrap();
        peer.apply_update(&delta).unwrap();
        apply_remote(store, store_id, node_id, &delta);
        assert_eq!(texts(P[0]), 2);
        assert_eq!(texts(P[1]), 2);

        // Undo is this pane's own: it takes back the first stroke, never the peer's.
        tools::undo(store, P[0], false);
        assert_eq!(texts(P[0]), 1);
        assert_eq!(texts(P[1]), 1);

        stop(store, P[0]);
        assert_eq!(store.panes_boarding(store_id, node_id), vec![P[1]]);
        stop(store, P[1]);
        assert!(!store.is_editing(store_id, node_id));
    }

    /// An image node's board: the picture at one unit per pixel in a locked
    /// layer of its own, and a layer above it to draw in.
    #[test]
    fn an_image_board_holds_its_picture_in_a_locked_layer() {
        let url = BlobUrl::parse("pimble-blob:00000000-0000-0000-0000-000000000001/aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let mut doc = NodeDoc::new();
        doc.init(node_types::BOARD, "Shot", None, "2026-10-09T10:00:00.000Z").unwrap();
        doc.apply_update(&new_board(Some((&url, 640, 480))).unwrap()).unwrap();
        let snapshot = Board::of_node(&doc).snapshot();
        assert_eq!(snapshot.layers.len(), 2);
        let picture = &snapshot.elements[0];
        assert!(snapshot.layer(&picture.layer).unwrap().locked);
        assert_eq!(picture.bounds, Rect::new(0.0, 0.0, 640.0, 480.0));
        assert!(!snapshot.top_layer().unwrap().locked, "new marks go above, into a layer that takes them");
        assert_eq!(snapshot.markup_count(), 0);
    }

    /// The next `PutBlob` sent, answered as stored at `size`.
    fn answer_put(store: AppStore, commands: &crossbeam_channel::Receiver<BackendCommand>, held: (StoreId, NodeId), n: u8, size: (u32, u32)) {
        let request_id = commands
            .try_iter()
            .find_map(|c| match c {
                BackendCommand::PutBlob { store_id, node_id, request_id, .. } => {
                    assert_eq!((store_id, node_id), held, "stored as a write of the board's node");
                    Some(request_id)
                }
                _ => None,
            })
            .expect("a picture is stored");
        let url = BlobUrl::parse(&format!("pimble-blob:00000000-0000-0000-0000-000000000001/{}", char::from(b'a' + n).to_string().repeat(25) + "a")).unwrap();
        assert!(stored(store, request_id, &Ok(StoredBlob { url, size: Some(size), notice: None })));
    }

    fn pictures_on(pane: PaneId) -> Vec<(Rect, String)> {
        with_session(pane, |s| {
            let snapshot = s.snapshot();
            snapshot.elements.iter().filter(|e| e.raw.kind.is_picture()).map(|e| (e.bounds, e.layer.clone())).collect()
        })
        .unwrap()
    }

    /// Pictures put on a board (contract section 1): stored one at a time as
    /// writes of the board's node, then placed together in one edit and one
    /// undo step, side by side in order, at one unit per pixel, centered in
    /// the view, in a layer that takes them, and selected.
    #[test]
    fn pictures_go_on_a_board_side_by_side_in_one_step() {
        let (store, commands) = store_with_commands();
        let held = (StoreId::new(), NodeId::new());
        start(store, P[0], held.0, held.1, &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(1600, 1200));
        let center = with_session(P[0], |s| s.view.center()).unwrap();
        let _ = commands.try_iter().count();

        add_to_board(store, P[0], None, vec![(Some("a.png".into()), b"a".to_vec()), (None, b"b".to_vec())]);
        answer_put(store, &commands, held, 1, (400, 300));
        assert!(pictures_on(P[0]).is_empty(), "nothing is placed until the last is stored");
        answer_put(store, &commands, held, 2, (200, 100));
        let placed = pictures_on(P[0]);
        assert_eq!(placed.len(), 2);
        let (a, b) = (placed[0].0, placed[1].0);
        assert_eq!((a.w, a.h, b.w, b.h), (400.0, 300.0, 200.0, 100.0), "one unit per pixel");
        assert_eq!(b.x - a.right(), 16.0, "side by side, in order");
        assert!(((a.x + b.right()) / 2.0 - center.x).abs() < 0.01, "centered in the view");
        assert!((a.y + a.h / 2.0 - center.y).abs() < 0.01 && (b.y + b.h / 2.0 - center.y).abs() < 0.01);
        let unlocked = with_session(P[0], |s| !s.snapshot().layer(&placed[0].1).unwrap().locked).unwrap();
        assert!(unlocked, "in a layer that takes them, so they can be moved");
        assert_eq!(with_session(P[0], |s| s.selected.len()).unwrap(), 2);
        assert_eq!(broadcasts(&commands), 1, "one applyEdit");
        assert_eq!(untracked(|| store.board_tool.get()), Tool::Select);

        tools::undo(store, P[0], false);
        assert!(pictures_on(P[0]).is_empty(), "one undo step");
        stop(store, P[0]);
    }

    /// A picture dropped at a point is centered there; one larger than the
    /// view is scaled down to fit it; an image node's board takes pictures
    /// beside its own, above its locked picture layer.
    #[test]
    fn a_dropped_picture_lands_where_it_was_dropped_and_fits_the_view() {
        let (store, commands) = store_with_commands();
        let held = (StoreId::new(), NodeId::new());
        let url = BlobUrl::parse("pimble-blob:00000000-0000-0000-0000-000000000001/aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let mut doc = NodeDoc::new();
        doc.init(node_types::BOARD, "Shot", None, "2026-10-09T10:00:00.000Z").unwrap();
        doc.apply_update(&new_board(Some((&url, 640, 480))).unwrap()).unwrap();
        start(store, P[0], held.0, held.1, &doc.save());
        with_session(P[0], |s| s.view.resize(800, 600));
        let _ = commands.try_iter().count();

        let at = with_session(P[0], |s| s.view.screen_to_board(100.0, 200.0)).unwrap();
        add_to_board(store, P[0], Some((100.0, 200.0)), vec![(None, b"a".to_vec())]);
        answer_put(store, &commands, held, 1, (40, 20));
        let placed = pictures_on(P[0]);
        assert_eq!(placed.len(), 2, "beside the image's own picture");
        let new = placed.iter().find(|(r, _)| r.w == 40.0).expect("at its own size").0;
        assert!((new.x + 20.0 - at.x).abs() < 0.01 && (new.y + 10.0 - at.y).abs() < 0.01, "centered on the drop");

        add_to_board(store, P[0], None, vec![(None, b"big".to_vec())]);
        answer_put(store, &commands, held, 2, (8000, 1000));
        let view_w = with_session(P[0], |s| {
            let a = s.view.screen_to_board(0.0, 0.0);
            let b = s.view.screen_to_board(800.0, 600.0);
            b.x - a.x
        })
        .unwrap();
        let big = pictures_on(P[0]).into_iter().map(|(r, _)| r).find(|r| r.w > 40.0 && r.w != 640.0).unwrap();
        assert!((big.w - view_w * 0.9).abs() < 0.5, "scaled down to fit the view: {} of {view_w}", big.w);
        assert!((big.w / big.h - 8.0).abs() < 0.01, "its proportions kept");
        stop(store, P[0]);
    }

    /// A board this device may only read takes no pictures, and stores none.
    #[test]
    fn a_read_only_board_takes_no_pictures() {
        let (store, commands) = store_with_commands();
        let held = (StoreId::new(), NodeId::new());
        start(store, P[0], held.0, held.1, &whiteboard_bytes());
        let mut node = pimble_core::Node::new(node_types::BOARD);
        node.id = held.1;
        node.access = pimble_core::StoreAccess::Read;
        store.node_data.update(|map| {
            map.insert(held, rinch::prelude::Signal::new(node));
        });
        let _ = commands.try_iter().count();
        add_to_board(store, P[0], None, vec![(None, b"a".to_vec())]);
        assert!(commands.try_iter().all(|c| !matches!(c, BackendCommand::PutBlob { .. })));
        assert_eq!(store.notice.get(), pimble_core::StoreAccess::READ_ONLY_REFUSAL);
        assert!(pictures_on(P[0]).is_empty());
        stop(store, P[0]);
    }

    /// A board closed (or showing another node) before its pictures were
    /// stored gets none of them, with a sentence.
    #[test]
    fn pictures_for_a_board_that_closed_are_not_placed() {
        let (store, commands) = store_with_commands();
        let held = (StoreId::new(), NodeId::new());
        start(store, P[0], held.0, held.1, &whiteboard_bytes());
        let _ = commands.try_iter().count();
        add_to_board(store, P[0], None, vec![(None, b"a".to_vec())]);
        stop(store, P[0]);
        let other = (StoreId::new(), NodeId::new());
        start(store, P[0], other.0, other.1, &whiteboard_bytes());
        answer_put(store, &commands, held, 1, (40, 20));
        assert!(pictures_on(P[0]).is_empty());
        assert!(store.notice.get().contains("closed"));
        stop(store, P[0]);
    }

    #[test]
    fn image_titles_come_from_file_names() {
        assert_eq!(image_title(Some("/tmp/Screenshot 1.png")), "Screenshot 1");
        assert_eq!(image_title(None), "Picture");
        assert_eq!(image_title(Some(".png")), ".png");
    }

    fn rect_element(x: f32) -> NewElement {
        NewElement::new(Kind::Rect, Geom::Rect(Rect::new(x, 0.0, 50.0, 50.0)))
    }

    fn element_count(pane: PaneId) -> usize {
        with_session(pane, |s| s.snapshot().elements.len()).unwrap()
    }

    /// Text is typed into a real input (contract section 4): the first
    /// character makes the element, later ones edit its yrs Text, and a text
    /// left empty goes when the typing ends.
    #[test]
    fn typing_makes_and_edits_a_text_element() {
        let (store, _commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        start(store, P[0], store_id, node_id, &whiteboard_bytes());
        tools::begin_text_new_for_test(store, P[0], Point::new(10.0, 10.0));
        assert_eq!(element_count(P[0]), 0, "nothing until a character is typed");
        tools::text_input(store, P[0], "Hi".to_string());
        tools::text_input(store, P[0], "Hi there".to_string());
        let text = with_session(P[0], |s| s.snapshot().elements[0].raw.text.clone()).unwrap();
        assert_eq!(text.as_deref(), Some("Hi there"));
        tools::text_input(store, P[0], String::new());
        tools::end_text(store, P[0]);
        assert_eq!(element_count(P[0]), 0, "an emptied text goes");
        stop(store, P[0]);
    }

    /// Groups (contract section 4): grouping makes one element the members
    /// name as parent; a duplicate copies the members too; ungrouping lets
    /// them go where they were; deleting a group deletes its members.
    #[test]
    fn grouping_duplicating_and_ungrouping() {
        let (store, _commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        start(store, P[0], store_id, node_id, &whiteboard_bytes());
        let a = local_edit(store, P[0], |t| t.add_element(rect_element(0.0))).unwrap();
        let b = local_edit(store, P[0], |t| t.add_element(rect_element(100.0))).unwrap();
        tools::select(store, P[0], vec![a.clone(), b.clone()]);
        tools::group(store, P[0]);
        let group = with_session(P[0], |s| s.selected.clone()).unwrap();
        assert_eq!(group.len(), 1);
        let parents = with_session(P[0], |s| [s.snapshot().get(&a).unwrap().parent.clone(), s.snapshot().get(&b).unwrap().parent.clone()]).unwrap();
        assert_eq!(parents, [Some(group[0].clone()), Some(group[0].clone())]);

        tools::duplicate(store, P[0]);
        assert_eq!(element_count(P[0]), 6, "a group and two members, twice");
        let copy = with_session(P[0], |s| s.selected.clone()).unwrap();
        assert_ne!(copy, group);
        tools::delete_selected(store, P[0]);
        assert_eq!(element_count(P[0]), 3, "the copy went with its members");

        tools::select(store, P[0], group.clone());
        tools::ungroup(store, P[0]);
        assert_eq!(element_count(P[0]), 2);
        let (pa, bounds) = with_session(P[0], |s| (s.snapshot().get(&a).unwrap().parent.clone(), s.snapshot().get(&b).unwrap().bounds)).unwrap();
        assert_eq!(pa, None);
        assert_eq!(bounds.x, 98.0, "it stayed where it was (its outline included)");
        stop(store, P[0]);
    }

    /// New elements go into the picked layer, and a locked one refuses them.
    #[test]
    fn a_locked_layer_takes_no_new_elements() {
        let (store, _commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        start(store, P[0], store_id, node_id, &whiteboard_bytes());
        let layer = with_session(P[0], |s| s.snapshot().layers[0].id.clone()).unwrap();
        local_edit(store, P[0], |t| t.set_layer_locked(&layer, true));
        tools::begin_text_new_for_test(store, P[0], Point::new(0.0, 0.0));
        tools::text_input(store, P[0], "x".to_string());
        assert_eq!(element_count(P[0]), 0);
        stop(store, P[0]);
    }

    fn pen(id: u64, pressure: f32) -> rinch::render_surface::SurfacePointer {
        rinch::render_surface::SurfacePointer { id, kind: rinch::render_surface::SurfacePointerKind::Pen, pressure, primary: true }
    }

    fn finger(id: u64) -> rinch::render_surface::SurfacePointer {
        rinch::render_surface::SurfacePointer { id, kind: rinch::render_surface::SurfacePointerKind::Touch, pressure: 0.5, primary: id == 10 }
    }

    fn broadcasts(commands: &crossbeam_channel::Receiver<BackendCommand>) -> usize {
        commands.try_iter().filter(|c| matches!(c, BackendCommand::BroadcastChanges { .. })).count()
    }

    /// The paint operations on pane 0's board, in drawing order.
    fn strokes() -> Vec<(String, std::sync::Arc<pimble_board::StrokeOp>)> {
        with_session(P[0], |s| {
            s.snapshot()
                .paint
                .iter()
                .filter_map(|op| match &op.raw.body {
                    pimble_board::PaintBody::Stroke(st) => Some((op.raw.layer.clone(), st.clone())),
                    _ => None,
                })
                .collect()
        })
        .unwrap()
    }

    /// Pointer events (rinch's `set_pointer_events`) on a paint tool: a
    /// pen's pressure is stored with each point, a mouse paints at full
    /// pressure; a cancelled stroke and a finger that turns out to be the
    /// first of a pinch leave nothing; the pen's eraser end erases whatever
    /// the tool; one gesture at a time. The highlighter paints into a
    /// multiply "Highlights" layer.
    #[test]
    fn pointers_paint_with_their_pressure_and_cancelled_strokes_leave_nothing() {
        use rinch::render_surface::{SurfacePointer, SurfacePointerKind};
        let left = SurfaceMouseButton::Left;
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        start(store, P[0], store_id, node_id, &whiteboard_bytes());
        store.board_tool.set(Tool::Highlighter);
        let _ = broadcasts(&commands);

        // A pen pressed harder as it goes.
        tools::pointer_down(store, P[0], 0.0, 0.0, left, pen(7, 0.2));
        for i in 1..=10 {
            tools::pointer_move(store, P[0], i as f32 * 8.0, 0.0, pen(7, 0.2 + i as f32 * 0.07));
        }
        // A second pointer's press meanwhile starts nothing.
        tools::pointer_down(store, P[0], 50.0, 50.0, left, SurfacePointer::mouse(true));
        tools::pointer_up(store, P[0], 50.0, 50.0, left, SurfacePointer::mouse(false));
        tools::pointer_up(store, P[0], 80.0, 0.0, left, pen(7, 0.0));
        let all = strokes();
        assert_eq!(all.len(), 1);
        let pressures: Vec<f32> = all[0].1.points.iter().map(|p| p[2]).collect();
        assert!(pressures.first().unwrap() < &0.35 && pressures.last().unwrap() > &0.8, "{pressures:?}");
        assert_eq!(all[0].1.preset, "highlighter");
        // Into "Highlights" (no picture here: the bottom layer, a
        // translucent normal layer, since multiply on bare canvas smears on
        // a dark theme).
        let layer = with_session(P[0], |s| s.snapshot().layer(&all[0].0).cloned()).flatten().unwrap();
        assert_eq!(layer.name, pimble_board::doc::HIGHLIGHTS_LAYER_NAME);
        assert_eq!(layer.blend, pimble_board::Blend::Normal);
        assert!((layer.opacity - pimble_board::BARE_HIGHLIGHTS_OPACITY).abs() < 1e-6);
        assert_eq!(layer.kind, pimble_board::LayerKind::Paint);
        // Begun on the press, the appends sent together about every 100 ms
        // (here, mostly on release), never one per point.
        let sent = broadcasts(&commands);
        assert!((2..=6).contains(&sent), "{sent}");

        // A mouse paints at full pressure, whatever it reports.
        let mut mouse = SurfacePointer::mouse(true);
        mouse.pressure = 0.5;
        tools::pointer_down(store, P[0], 0.0, 40.0, left, mouse);
        tools::pointer_move(store, P[0], 30.0, 40.0, mouse);
        tools::pointer_up(store, P[0], 30.0, 40.0, left, SurfacePointer::mouse(false));
        let all = strokes();
        assert_eq!(all.len(), 2);
        assert!(all[1].1.points.iter().all(|p| p[2] == 1.0), "{:?}", all[1].1.points);
        assert_eq!(all[1].0, all[0].0, "the same Highlights layer, reused");

        // A stroke the platform cancels is gone.
        tools::pointer_down(store, P[0], 0.0, 80.0, left, pen(7, 0.5));
        tools::pointer_move(store, P[0], 40.0, 80.0, pen(7, 0.6));
        tools::pointer_cancel(store, P[0], pen(7, 0.0));
        assert_eq!(strokes().len(), 2);
        assert!(with_session(P[0], |s| matches!(s.gesture, Gesture::Idle) && s.driver.is_none() && s.outbox.is_empty()).unwrap());

        // One finger starts a stroke; a second makes it a pinch: the stroke
        // goes, and moving the fingers pans.
        tools::pointer_down(store, P[0], 0.0, 120.0, left, finger(10));
        tools::pointer_move(store, P[0], 10.0, 120.0, finger(10));
        tools::pointer_down(store, P[0], 100.0, 120.0, left, finger(11));
        assert!(with_session(P[0], |s| matches!(s.gesture, Gesture::Pinch)).unwrap());
        let before = with_session(P[0], |s| s.view.screen_to_board(0.0, 0.0)).unwrap();
        tools::pointer_move(store, P[0], 30.0, 140.0, finger(10));
        tools::pointer_move(store, P[0], 120.0, 140.0, finger(11));
        let after = with_session(P[0], |s| s.view.screen_to_board(0.0, 0.0)).unwrap();
        let zoom = with_session(P[0], |s| s.view.zoom()).unwrap();
        assert!(((before.x - after.x) * zoom - 20.0).abs() < 1e-3 && ((before.y - after.y) * zoom - 20.0).abs() < 1e-3, "{before:?} {after:?}");
        tools::pointer_up(store, P[0], 30.0, 140.0, left, finger(10));
        tools::pointer_move(store, P[0], 200.0, 200.0, finger(11));
        tools::pointer_up(store, P[0], 200.0, 200.0, left, finger(11));
        assert_eq!(strokes().len(), 2);
        assert!(with_session(P[0], |s| matches!(s.gesture, Gesture::Idle) && s.touches.is_empty()).unwrap());

        // The pen's eraser end erases with the highlighter still chosen:
        // nothing picked and no element there, so the paint layer under it,
        // with an erasing stroke.
        let on_first = on_screen(Point::new(all[0].1.points[3][0], all[0].1.points[3][1]));
        let eraser = SurfacePointer { kind: SurfacePointerKind::Eraser, ..pen(8, 0.5) };
        tools::pointer_down(store, P[0], on_first.0, on_first.1, left, eraser);
        tools::pointer_move(store, P[0], on_first.0 + 10.0, on_first.1, eraser);
        tools::pointer_up(store, P[0], on_first.0 + 10.0, on_first.1, left, eraser);
        let all = strokes();
        assert_eq!(all.len(), 3);
        assert_eq!(all[2].1.blend, pimble_board::StrokeBlend::Erase);
        assert_eq!(all[2].0, all[0].0);
        assert_eq!(untracked(|| store.board_tool.get()), Tool::Highlighter);
        stop(store, P[0]);
    }

    /// The Brush (docs/DRAWING_CONTRACT.md section 3): a stroke goes into a
    /// new "Paint 1" made with it (picked in the panel) in one undo step,
    /// with the preset and settings of the context bar; the next stroke
    /// goes into the picked layer; `]` makes the brush larger; the eraser
    /// on the picked paint layer erases with a stroke; the eyedropper picks
    /// the color under the pointer into the well's front square.
    #[test]
    fn the_brush_paints_into_a_paint_layer_in_one_undo_step() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        start(store, P[0], store_id, node_id, &whiteboard_bytes());
        let st = store.board_style;
        st.stroke.set("#00c000".to_string());
        st.brush.set("ink");
        store.board_tool.set(Tool::Brush);
        let layers_before = with_session(P[0], |s| s.snapshot().layers.len()).unwrap();
        let _ = broadcasts(&commands);

        mouse_drag(store, (100.0, 100.0), (300.0, 100.0));
        let all = strokes();
        assert_eq!(all.len(), 1);
        let (layer_id, stroke) = &all[0];
        assert_eq!(stroke.preset, "ink");
        assert_eq!(stroke.color.to_hex(), "#00c000");
        assert!(stroke.points.len() >= 5, "{:?}", stroke.points);
        let layer = with_session(P[0], |s| s.snapshot().layer(layer_id).cloned()).flatten().unwrap();
        assert_eq!((layer.name.as_str(), layer.kind.clone()), ("Paint 1", pimble_board::LayerKind::Paint));
        assert_eq!(untracked(|| store.pane(P[0]).board_ui.layer.get()).as_deref(), Some(layer_id.as_str()));
        assert_eq!(with_session(P[0], |s| s.snapshot().layers.len()).unwrap(), layers_before + 1);
        let sent = broadcasts(&commands);
        assert!((2..=4).contains(&sent), "begun on the press, the rest batched: {sent}");

        // `]`: larger; the next stroke takes the new size, in the same layer.
        let size = untracked(|| paint::current_settings(store)).unwrap().size;
        paint::step_size(store, P[0], true);
        let larger = untracked(|| paint::current_settings(store)).unwrap().size;
        assert!(larger > size);
        mouse_drag(store, (100.0, 200.0), (300.0, 200.0));
        let all = strokes();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].0, *layer_id);
        assert_eq!(all[1].1.settings.size, larger);

        // The eyedropper picks the green under the first stroke into the
        // front square (the stroke's), and the recent colors.
        st.stroke.set("#000000".to_string());
        store.board_tool.set(Tool::Eyedropper);
        let at = on_screen(Point::new(200.0, 100.0));
        mouse_click(store, at);
        let picked = Color::parse(&untracked(|| st.stroke.get())).unwrap();
        assert!(picked.g > 150 && picked.r < 60 && picked.b < 60, "{picked:?}");
        assert_eq!(untracked(|| st.recent.get()).first(), Some(&picked.to_hex()));

        // The eraser on the picked paint layer erases with a stroke.
        store.board_tool.set(Tool::Eraser);
        mouse_drag(store, (150.0, 100.0), (250.0, 100.0));
        let all = strokes();
        assert_eq!(all.len(), 3);
        assert_eq!(all[2].1.blend, pimble_board::StrokeBlend::Erase);
        assert_eq!(all[2].0, *layer_id);

        // Undo takes each stroke back whole, then the first with its layer.
        tools::undo(store, P[0], false);
        assert_eq!(strokes().len(), 2);
        tools::undo(store, P[0], false);
        assert_eq!(strokes().len(), 1);
        tools::undo(store, P[0], false);
        assert_eq!(strokes().len(), 0);
        assert_eq!(with_session(P[0], |s| s.snapshot().layers.len()).unwrap(), layers_before, "the layer went with its first stroke");
        stop(store, P[0]);
    }

    /// Alt held with a brush picks the color under the press instead of
    /// painting.
    #[test]
    fn alt_with_a_brush_picks_a_color() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        let st = store.board_style;
        st.stroke.set("#2050e0".to_string());
        store.board_tool.set(Tool::Brush);
        mouse_drag(store, on_screen(Point::new(0.0, 0.0)), on_screen(Point::new(200.0, 0.0)));
        assert_eq!(strokes().len(), 1);
        st.stroke.set("#000000".to_string());
        let alt = rinch::render_surface::SurfaceKeyData { key: "Alt".into(), code: "AltLeft".into(), ctrl: false, shift: false, alt: true, meta: false };
        tools::on_event(store, P[0], SurfaceEvent::KeyDown(alt.clone()));
        mouse_click(store, on_screen(Point::new(100.0, 0.0)));
        assert_eq!(strokes().len(), 1, "nothing painted");
        assert_eq!(untracked(|| st.stroke.get()), "#2050e0");
        tools::on_event(store, P[0], SurfaceEvent::KeyUp(alt));
        assert_eq!(untracked(|| store.board_tool.get()), Tool::Brush);
        stop(store, P[0]);
    }

    /// The highlighter over a picture paints into a "Highlights" layer
    /// right above the picture's (multiply), and hiding the markup hides it.
    #[test]
    fn the_highlighter_marks_a_picture_in_a_highlights_layer() {
        let url = BlobUrl::parse("pimble-blob:00000000-0000-0000-0000-000000000001/aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let mut doc = NodeDoc::new();
        doc.init(node_types::BOARD, "Shot", None, "2026-10-09T10:00:00.000Z").unwrap();
        doc.apply_update(&new_board(Some((&url, 640, 480))).unwrap()).unwrap();
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &doc.save());
        store.board_tool.set(Tool::Highlighter);
        let from = on_screen(Point::new(100.0, 100.0));
        let to = on_screen(Point::new(300.0, 100.0));
        mouse_drag(store, from, to);
        let all = strokes();
        assert_eq!(all.len(), 1);
        let (picture_layer, rows) = with_session(P[0], |s| {
            let snapshot = s.snapshot();
            (snapshot.elements[0].layer.clone(), layers::rows_of(snapshot))
        })
        .unwrap();
        // Top first: the Highlights row sits right above the picture's.
        let at = rows.iter().position(|r| r.id == picture_layer).unwrap();
        assert!(at > 0);
        assert_eq!(rows[at - 1].id, all[0].0);
        assert_eq!(rows[at - 1].name, pimble_board::doc::HIGHLIGHTS_LAYER_NAME);
        assert_eq!(rows[at - 1].blend, pimble_board::Blend::Multiply);
        // Its yellow, not the marks' red.
        assert_eq!(all[0].1.color, pimble_board::brush::HIGHLIGHTER.color);
        // Hiding the markup hides it.
        toggle_markup(store, P[0]);
        let shown = with_session(P[0], |s| {
            let snapshot = s.snapshot();
            let i = snapshot.layers.iter().position(|l| l.id == all[0].0).unwrap();
            snapshot.paint_shown(i)
        })
        .unwrap();
        assert!(!shown);
        stop(store, P[0]);
    }

    fn mouse_click(store: AppStore, (x, y): (f32, f32)) {
        use rinch::render_surface::SurfacePointer;
        tools::pointer_down(store, P[0], x, y, SurfaceMouseButton::Left, SurfacePointer::mouse(true));
        tools::pointer_up(store, P[0], x, y, SurfaceMouseButton::Left, SurfacePointer::mouse(false));
    }

    fn mouse_drag(store: AppStore, from: (f32, f32), to: (f32, f32)) {
        use rinch::render_surface::SurfacePointer;
        tools::pointer_down(store, P[0], from.0, from.1, SurfaceMouseButton::Left, SurfacePointer::mouse(true));
        for i in 1..=4 {
            let k = i as f32 / 4.0;
            tools::pointer_move(store, P[0], from.0 + (to.0 - from.0) * k, from.1 + (to.1 - from.1) * k, SurfacePointer::mouse(true));
        }
        tools::pointer_up(store, P[0], to.0, to.1, SurfaceMouseButton::Left, SurfacePointer::mouse(false));
    }

    /// Board point `p` on pane 0's surface, physical pixels.
    fn on_screen(p: Point) -> (f32, f32) {
        with_session(P[0], |s| s.view.board_to_screen(p)).unwrap()
    }

    /// An element as a test reads it.
    struct Seen {
        raw: pimble_board::RawElement,
        layer: String,
    }

    fn only_element() -> Seen {
        with_session(P[0], |s| {
            assert_eq!(s.snapshot().elements.len(), 1);
            let e = &s.snapshot().elements[0];
            Seen { raw: e.raw.clone(), layer: e.layer.clone() }
        })
        .unwrap()
    }

    fn path_of(e: &Seen) -> pimble_board::PathGeom {
        match &e.raw.geom {
            Geom::Path(p) => p.clone(),
            other => panic!("not a path: {other:?}"),
        }
    }

    /// The Pen (docs/DRAWING_CONTRACT.md section 2): clicks place corners,
    /// a press-drag a smooth anchor, a click on the first anchor closes the
    /// path; the element is written from its second anchor on, and the
    /// whole path is one undo step.
    #[test]
    fn the_pen_draws_a_closed_curve_in_one_undo_step() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(800, 600));
        store.board_tool.set(Tool::Pen);
        let a = on_screen(Point::new(0.0, 0.0));
        mouse_click(store, a);
        assert_eq!(element_count(P[0]), 0, "one anchor is not written");
        let b = on_screen(Point::new(200.0, 0.0));
        mouse_drag(store, b, (b.0 + 50.0, b.1 + 50.0));
        let e = only_element();
        assert_eq!(e.raw.kind, Kind::Path);
        let path = path_of(&e);
        assert_eq!(path.anchor_count(), 2);
        let curve = path.subpaths[0].anchors[1];
        assert!((curve.h_out.x - 50.0).abs() < 0.01 && (curve.h_out.y - 50.0).abs() < 0.01, "{curve:?}");
        assert_eq!(curve.mode, pimble_board::AnchorMode::Symmetric);
        // Hovering shows the segment to the pointer; it writes nothing.
        tools::pointer_move(store, P[0], 300.0, 400.0, rinch::render_surface::SurfacePointer::mouse(false));
        mouse_click(store, on_screen(Point::new(100.0, 150.0)));
        mouse_click(store, on_screen(Point::new(1.0, -1.0)));
        let e = only_element();
        let path = path_of(&e);
        assert_eq!(path.anchor_count(), 3);
        assert!(path.subpaths[0].closed, "a click on the first anchor closes it");
        assert!(!points::drawing(P[0]), "closing finishes the path");
        assert_eq!(with_session(P[0], |s| s.selected.clone()).unwrap(), vec![e.raw.id.clone()]);

        tools::undo(store, P[0], false);
        assert_eq!(element_count(P[0]), 0, "the whole path is one undo step");
        stop(store, P[0]);
    }

    /// Escape finishes an open Pen path as it is; a path of one anchor is
    /// never written.
    #[test]
    fn escape_finishes_an_open_pen_path() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(800, 600));
        store.board_tool.set(Tool::Pen);
        let escape = rinch::render_surface::SurfaceKeyData { key: "Escape".into(), code: "Escape".into(), ctrl: false, shift: false, alt: false, meta: false };
        mouse_click(store, on_screen(Point::new(0.0, 0.0)));
        tools::on_event(store, P[0], SurfaceEvent::KeyDown(escape.clone()));
        assert!(!points::drawing(P[0]));
        assert_eq!(element_count(P[0]), 0);
        mouse_click(store, on_screen(Point::new(0.0, 0.0)));
        mouse_click(store, on_screen(Point::new(100.0, 0.0)));
        mouse_click(store, on_screen(Point::new(100.0, 100.0)));
        tools::on_event(store, P[0], SurfaceEvent::KeyDown(escape));
        let path = path_of(&only_element());
        assert_eq!(path.anchor_count(), 3);
        assert!(!path.subpaths[0].closed);
        assert!(!points::drawing(P[0]));
        stop(store, P[0]);
    }

    /// The Pencil streams a pen stroke while drawing (others watch it) and
    /// leaves a path fitted to it on release; one released at once is
    /// written as that path directly.
    #[test]
    fn the_pencil_streams_a_stroke_and_leaves_a_path() {
        use rinch::render_surface::SurfacePointer;
        let left = SurfaceMouseButton::Left;
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(800, 600));
        store.board_tool.set(Tool::Pencil);
        tools::pointer_down(store, P[0], 100.0, 100.0, left, SurfacePointer::mouse(true));
        for i in 1..=20 {
            let x = 100.0 + i as f32 * 10.0;
            let y = 100.0 + (i as f32 * 0.4).sin() * 40.0;
            tools::pointer_move(store, P[0], x, y, SurfacePointer::mouse(true));
            if i == 10 {
                std::thread::sleep(std::time::Duration::from_millis(APPEND_EVERY_MS as u64 + 20));
            }
        }
        assert_eq!(only_element().raw.kind, Kind::Pen, "streamed as a pen stroke");
        tools::pointer_up(store, P[0], 300.0, 100.0, left, SurfacePointer::mouse(false));
        let e = only_element();
        assert_eq!(e.raw.kind, Kind::Path, "a path once released");
        assert!(e.raw.points.is_empty());
        let n = path_of(&e).anchor_count();
        assert!(n >= 2 && n < 20, "fitted to a few anchors: {n}");

        // Released before anything streamed.
        mouse_drag(store, (100.0, 300.0), (300.0, 320.0));
        let kinds: Vec<Kind> = with_session(P[0], |s| s.snapshot().elements.iter().map(|e| e.raw.kind.clone()).collect()).unwrap();
        assert_eq!(kinds, vec![Kind::Path, Kind::Path]);
        stop(store, P[0]);
    }

    /// Direct select on a live rectangle: dragging a corner turns it into a
    /// path in that same edit (one undo takes both back).
    #[test]
    fn direct_select_turns_a_shape_into_a_path_when_a_point_moves() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(800, 600));
        let id = local_edit(store, P[0], |t| t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(100.0, 100.0, 200.0, 100.0))))).unwrap();
        store.board_tool.set(Tool::DirectSelect);
        let corner = on_screen(Point::new(100.0, 100.0));
        mouse_drag(store, corner, (corner.0 + 40.0, corner.1));
        let e = only_element();
        assert_eq!(e.raw.id, id, "it keeps its id");
        assert_eq!(e.raw.kind, Kind::Path);
        let path = path_of(&e);
        assert_eq!(path.anchor_count(), 4);
        let anchors: Vec<Point> = path.subpaths[0].anchors.iter().map(|a| a.p).collect();
        assert!(anchors.iter().any(|p| (p.x - 140.0).abs() < 0.01 && (p.y - 100.0).abs() < 0.01), "{anchors:?}");
        assert!(anchors.iter().any(|p| (p.x - 300.0).abs() < 0.01 && (p.y - 200.0).abs() < 0.01), "the rest stay");
        assert!(points::anchors_selected(P[0]));

        // Delete removes the selected anchor.
        points::delete_anchors(store, P[0]);
        assert_eq!(path_of(&only_element()).anchor_count(), 3);
        tools::undo(store, P[0], false);
        assert_eq!(path_of(&only_element()).anchor_count(), 4);
        tools::undo(store, P[0], false);
        assert_eq!(only_element().raw.kind, Kind::Rect, "one undo takes the move and the conversion back");
        stop(store, P[0]);
    }

    /// Direct select: Shift keeps a dragged anchor at a multiple of 45°
    /// from where it was, and with snapping on it lands on the grid.
    #[test]
    fn direct_select_constrains_and_snaps_points() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(800, 600));
        local_edit(store, P[0], |t| t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(100.0, 100.0, 200.0, 100.0)))));
        store.board_tool.set(Tool::DirectSelect);
        let corner = on_screen(Point::new(100.0, 100.0));
        with_session(P[0], |s| s.shift = true);
        mouse_drag(store, corner, (corner.0 + 50.0, corner.1 + 38.0));
        with_session(P[0], |s| s.shift = false);
        let moved = path_of(&only_element()).subpaths[0].anchors.iter().map(|a| a.p).find(|p| p.x > 101.0 && p.y > 101.0 && p.x < 200.0).unwrap();
        assert!(((moved.x - 100.0) - (moved.y - 100.0)).abs() < 0.01, "on the diagonal: {moved:?}");

        // Snapping: the pressed anchor lands on the grid.
        store.board_style.snap.set(true);
        let from = on_screen(moved);
        mouse_drag(store, from, (from.0 + 33.0, from.1 + 7.0));
        let anchors: Vec<Point> = path_of(&only_element()).subpaths[0].anchors.iter().map(|a| a.p).collect();
        let on_grid = |v: f32| ((v / GRID).round() * GRID - v).abs() < 0.01;
        assert!(anchors.iter().any(|p| on_grid(p.x) && on_grid(p.y) && p.x > 100.0 && p.x < 300.0 && p.y > 100.0), "{anchors:?}");
        stop(store, P[0]);
    }

    /// Direct select across elements: a marquee picks anchors of two
    /// rectangles, a drag of one of them moves them all, and Delete takes
    /// them from both in one undo step.
    #[test]
    fn direct_select_moves_anchors_of_several_elements() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(800, 600));
        let a = local_edit(store, P[0], |t| t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(100.0, 100.0, 100.0, 100.0))))).unwrap();
        let b = local_edit(store, P[0], |t| t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(300.0, 100.0, 100.0, 100.0))))).unwrap();
        store.board_tool.set(Tool::DirectSelect);
        mouse_drag(store, on_screen(Point::new(50.0, 50.0)), on_screen(Point::new(450.0, 150.0)));
        let (shown, others) = with_session(P[0], |s| {
            let pe = s.points.as_ref().unwrap();
            (pe.anchors.len(), pe.others.iter().map(|o| o.anchors.len()).collect::<Vec<_>>())
        })
        .unwrap();
        assert_eq!((shown, others), (2, vec![2]), "the top corners of both");
        let geom_of = |id: &str| with_session(P[0], |s| s.snapshot().get(id).unwrap().raw.geom.clone()).unwrap();
        let tops = |id: &str| match geom_of(id) {
            Geom::Path(p) => p.subpaths[0].anchors.iter().filter(|a| a.p.y < 99.0).count(),
            _ => 0,
        };
        let from = on_screen(Point::new(100.0, 100.0));
        mouse_drag(store, from, (from.0, from.1 - 30.0));
        assert_eq!((tops(&a), tops(&b)), (2, 2), "both rectangles' top corners moved up");

        points::delete_anchors(store, P[0]);
        let count = |id: &str| match geom_of(id) {
            Geom::Path(p) => p.anchor_count(),
            _ => 4,
        };
        assert_eq!((count(&a), count(&b)), (2, 2));
        tools::undo(store, P[0], false);
        assert_eq!((count(&a), count(&b)), (4, 4), "one undo puts the anchors back on both");
        tools::undo(store, P[0], false);
        assert!(matches!(geom_of(&a), Geom::Rect(_)) && matches!(geom_of(&b), Geom::Rect(_)), "one undo for the drag of both");
        stop(store, P[0]);
    }

    /// Shapes stay live: a star's points and a rectangle's radius change
    /// from the context bar, each only on the kinds that have them.
    #[test]
    fn live_shapes_take_their_own_parameters() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        let star = local_edit(store, P[0], |t| t.add_element(NewElement::new(Kind::Star, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 100.0))))).unwrap();
        let rect = local_edit(store, P[0], |t| t.add_element(rect_element(200.0))).unwrap();
        tools::select(store, P[0], vec![star.clone(), rect.clone()]);
        tools::apply_style(store, P[0], Style { points: Some(8), ..Style::default() });
        tools::apply_style(store, P[0], Style { radius: Some(12.0), ..Style::default() });
        let (star_e, rect_e) = with_session(P[0], |s| (s.snapshot().get(&star).unwrap().raw.clone(), s.snapshot().get(&rect).unwrap().raw.clone())).unwrap();
        assert_eq!(star_e.kind, Kind::Star);
        assert_eq!(star_e.style.points, Some(8));
        assert_eq!(star_e.style.radius, None, "a star has no corner radius");
        assert_eq!(rect_e.style.radius, Some(12.0));
        assert_eq!(rect_e.style.points, None);
        // A dash preset clears a custom pattern, which would still win.
        local_edit(store, P[0], |t| t.set_style(&rect, &Style { dash_pattern: Some(vec![6.0, 3.0]), ..Style::default() }));
        tools::apply_style(store, P[0], Style { dash: Some(Dash::Dashed), ..Style::default() });
        let pattern = with_session(P[0], |s| s.snapshot().get(&rect).unwrap().raw.style.dash_pattern.clone()).unwrap();
        assert_eq!(pattern, None);
        stop(store, P[0]);
    }

    /// The miter limit and a custom dash pattern: new paths take them, the
    /// miter only while corners are mitered, and the selection is written.
    #[test]
    fn miter_limit_and_dash_pattern_reach_new_and_selected_elements() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        let st = store.board_style;
        st.miter.set(10.0);
        st.dash_text.set("6 3 1 3".into());
        let round = tools::new_style(store, &Kind::Path);
        assert_eq!(round.miter, None, "no miter limit without mitered corners");
        assert_eq!(round.dash_pattern, Some(vec![6.0, 3.0, 1.0, 3.0]));
        st.join.set(Some(pimble_board::Join::Miter));
        assert_eq!(tools::new_style(store, &Kind::Path).miter, Some(10.0));
        assert_eq!(tools::new_style(store, &Kind::Line).miter, None, "a line has no corners");
        st.dash_text.set("6 x".into());
        assert_eq!(tools::new_style(store, &Kind::Rect).dash_pattern, None, "an unreadable pattern is none");

        let rect = local_edit(store, P[0], |t| t.add_element(rect_element(0.0))).unwrap();
        tools::select(store, P[0], vec![rect.clone()]);
        tools::apply_style(store, P[0], Style { miter: Some(8.0), dash_pattern: Some(vec![4.0, 2.0]), ..Style::default() });
        let raw = with_session(P[0], |s| s.snapshot().get(&rect).unwrap().raw.style.clone()).unwrap();
        assert_eq!((raw.miter, raw.dash_pattern.clone()), (Some(8.0), Some(vec![4.0, 2.0])));
        // Selecting it shows them in the bar.
        tools::load_selection_style(store, P[0]);
        assert_eq!((st.miter.get(), st.dash_text.get()), (8.0, "4 2".to_string()));
        tools::clear_dash_pattern(store, P[0]);
        let raw = with_session(P[0], |s| s.snapshot().get(&rect).unwrap().raw.style.clone()).unwrap();
        assert_eq!(raw.dash_pattern, None);
        stop(store, P[0]);
    }

    /// Outline Stroke from the selection's "more" menu: the line becomes a
    /// filled path, selected, in one undo step; a text is refused with a
    /// sentence and nothing changes.
    #[test]
    fn outline_stroke_turns_the_selection_into_filled_outlines() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        let line = local_edit(store, P[0], |t| {
            t.add_element(NewElement::new(Kind::Line, Geom::Segment { a: Point::new(0.0, 0.0), b: Point::new(100.0, 0.0) }).with_style(Style {
                width: Some(6.0),
                arrow_heads: Some(ArrowHeads::None),
                ..Style::default()
            }))
        })
        .unwrap();
        tools::select(store, P[0], vec![line.clone()]);
        tools::outline_stroke(store, P[0]);
        let e = only_element();
        assert_eq!(e.raw.kind, Kind::Path);
        assert!(e.raw.style.fill.is_some());
        assert_eq!(with_session(P[0], |s| s.selected.clone()).unwrap(), vec![line.clone()]);
        tools::undo(store, P[0], false);
        assert_eq!(only_element().raw.kind, Kind::Line, "one undo");
        stop(store, P[0]);
    }

    /// Map lettering (docs/DRAWING_WAVE6_CONTRACT.md section 4): Put Text
    /// on Path with a text and a line selected, the text options writing
    /// font, halo and spacing, the place along the path, duplicating both
    /// together, and Release from Path.
    #[test]
    fn put_text_on_path_and_letter_it() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        let (line, label) = local_edit(store, P[0], |t| {
            let line = t.add_element(NewElement::new(Kind::Line, Geom::Segment { a: Point::new(0.0, 200.0), b: Point::new(400.0, 200.0) }))?;
            let label = t.add_element(NewElement::new(Kind::Text, Geom::Rect(Rect::new(0.0, 0.0, 0.0, 0.0))).with_text("Mill Road"))?;
            Ok((line, label))
        })
        .unwrap();
        let snap = || with_session(P[0], |s| s.snapshot().get(&label).map(|e| (e.raw.style.clone(), e.along.is_some(), e.bounds))).flatten().unwrap();
        // One selected is not enough.
        tools::select(store, P[0], vec![label.clone()]);
        tools::put_text_on_path(store, P[0]);
        assert!(!snap().1);
        tools::select(store, P[0], vec![line.clone(), label.clone()]);
        tools::put_text_on_path(store, P[0]);
        let (style, placed, bounds) = snap();
        assert_eq!(style.along.as_deref(), Some(line.as_str()));
        assert!(placed && bounds.bottom() < 202.0 && bounds.y > 150.0, "{bounds:?}");
        assert_eq!(with_session(P[0], |s| s.selected.clone()).unwrap(), vec![label.clone()], "the text stays selected");
        assert!(untracked(|| store.board_style.on_path.get()));
        // The text options.
        tools::apply_style(store, P[0], Style { font: Some(pimble_board::FontFamily::Serif), bold: Some(true), tracking: Some(0.25), along_side: Some(pimble_board::AlongSide::Below), ..Style::default() });
        tools::apply_halo(store, P[0], 0.12, "#ffffff");
        let (style, ..) = snap();
        assert_eq!((style.font, style.bold, style.tracking), (Some(pimble_board::FontFamily::Serif), Some(true), Some(0.25)));
        assert_eq!(style.along_side, Some(pimble_board::AlongSide::Below));
        assert!((style.halo_width.unwrap() - 0.12 * 24.0).abs() < 1e-4 && style.halo_color == Some(Color::WHITE));
        tools::apply_halo(store, P[0], 0.0, "#ffffff");
        assert_eq!(snap().0.halo_width, None);
        // Duplicated with its line, the copy follows the copied line.
        tools::select(store, P[0], vec![line.clone(), label.clone()]);
        tools::duplicate(store, P[0]);
        let copies = with_session(P[0], |s| s.selected.clone()).unwrap();
        let (copy_line, copy_label) = (copies[0].clone(), copies[1].clone());
        let along = with_session(P[0], |s| s.snapshot().get(&copy_label).and_then(|e| e.raw.style.along.clone())).flatten();
        assert_eq!(along, Some(copy_line));
        // Release from Path.
        tools::select(store, P[0], vec![label.clone()]);
        tools::release_from_path(store, P[0]);
        let (style, placed, _) = snap();
        assert!(!placed && style.along.is_none() && style.along_side.is_none());
        assert_eq!(style.font, Some(pimble_board::FontFamily::Serif), "the lettering stays");
        tools::undo(store, P[0], false);
        assert!(snap().1, "one undo puts it back on");
        stop(store, P[0]);
    }

    /// Tools pick their layer (docs/DRAWING_CONTRACT.md section 1): with no
    /// vector layer to draw in, the Pen makes "Vector 1" and draws there.
    #[test]
    fn the_pen_makes_a_vector_layer_when_there_is_none() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(800, 600));
        let vector = with_session(P[0], |s| s.snapshot().layers[0].id.clone()).unwrap();
        local_edit(store, P[0], |t| {
            t.add_layer_in("Paint 1", pimble_board::LayerKind::Paint, None, Placement::Top)?;
            t.delete_layer(&vector)
        })
        .unwrap();
        store.board_tool.set(Tool::Pen);
        mouse_click(store, on_screen(Point::new(0.0, 0.0)));
        mouse_click(store, on_screen(Point::new(100.0, 0.0)));
        let e = only_element();
        let (name, vector) = with_session(P[0], |s| {
            let l = s.snapshot().layer(&e.layer).unwrap();
            (l.name.clone(), l.kind.is_vector())
        })
        .unwrap();
        assert_eq!(name, "Vector 1");
        assert!(vector);
        stop(store, P[0]);
    }

    fn ellipse_element(x: f32) -> NewElement {
        NewElement::new(Kind::Ellipse, Geom::Rect(Rect::new(x, 0.0, 100.0, 100.0)))
    }

    /// Combining (docs/DRAWING_CONTRACT.md section 2): two overlapping
    /// circles become one path, selected; a text refuses with the board's
    /// sentence and nothing changes.
    #[test]
    fn combining_selects_the_result_and_says_a_refusal() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        let a = local_edit(store, P[0], |t| t.add_element(ellipse_element(0.0))).unwrap();
        let b = local_edit(store, P[0], |t| t.add_element(ellipse_element(50.0))).unwrap();
        tools::select(store, P[0], vec![a.clone(), b.clone()]);
        tools::combine(store, P[0], pimble_board::CombineOp::Union);
        let e = only_element();
        assert_eq!(e.raw.kind, Kind::Path);
        assert_eq!(with_session(P[0], |s| s.selected.clone()).unwrap(), vec![e.raw.id.clone()]);
        // One undo brings both circles back.
        tools::undo(store, P[0], false);
        assert_eq!(element_count(P[0]), 2);

        let text = local_edit(store, P[0], |t| t.add_element(NewElement::new(Kind::Text, Geom::Rect(Rect::new(0.0, 200.0, 80.0, 30.0))).with_text("Hi"))).unwrap();
        tools::select(store, P[0], vec![a.clone(), text.clone()]);
        tools::combine(store, P[0], pimble_board::CombineOp::Subtract);
        assert_eq!(element_count(P[0]), 3, "nothing written");
        assert!(untracked(|| store.notice.get()).contains("combine"), "{}", untracked(|| store.notice.get()));
        stop(store, P[0]);
    }

    /// The color well writes paints: a gradient picked replaces the solid
    /// fill, a solid color picked after it takes the gradient away (never
    /// the solid key alone), and new shapes take the well's gradient.
    #[test]
    fn the_well_writes_gradients_and_solid_colors_replace_them() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        let rect = local_edit(store, P[0], |t| t.add_element(rect_element(0.0))).unwrap();
        tools::select(store, P[0], vec![rect.clone()]);
        let st = store.board_style;
        st.front.set(rail::Well::Fill);
        gradient::switch_to(store, P[0], Some(pimble_board::GradientKind::Linear));
        let fill = |id: &str| with_session(P[0], |s| s.snapshot().get(id).unwrap().style.fill_gradient.clone()).unwrap();
        let g = fill(&rect).expect("a gradient fill");
        assert_eq!(g.kind, pimble_board::GradientKind::Linear);
        // A stop added, and the radial switch keeps the stops.
        gradient::press_bar(store, P[0], 0.5);
        assert_eq!(untracked(|| st.stop.get()), 1);
        assert_eq!(fill(&rect).unwrap().stops.len(), 3);
        gradient::switch_to(store, P[0], Some(pimble_board::GradientKind::Radial));
        let radial = fill(&rect).unwrap();
        assert_eq!((radial.kind, radial.stops.len()), (pimble_board::GradientKind::Radial, 3));
        // The picked stop takes a picked color.
        assert!(gradient::color_picked(store, P[0], "#00ff00"));
        assert_eq!(fill(&rect).unwrap().stops[1].color, Color::rgb(0, 255, 0));
        // New shapes take the well's gradient.
        let style = tools::new_style(store, &Kind::Rect);
        assert_eq!(style.fill_gradient.map(|g| g.kind), Some(pimble_board::GradientKind::Radial));
        // A solid color replaces it.
        rail::set_color(store, P[0], rail::Well::Fill, Some("#123456"));
        assert_eq!(fill(&rect), None);
        let solid = with_session(P[0], |s| s.snapshot().get(&rect).unwrap().style.fill).unwrap();
        assert_eq!(solid, Color::parse("#123456"));
        assert_eq!(untracked(|| st.fill_gradient.get()), None);
        stop(store, P[0]);
    }

    /// The gradient's end handle shows while the well's picker is open and
    /// drags with the Select tool, as one undo step.
    #[test]
    fn a_gradient_handle_drags_on_the_canvas() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(800, 600));
        let rect = local_edit(store, P[0], |t| {
            let id = t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 200.0, 100.0))))?;
            t.set_fill_paint(&id, &pimble_board::Paint::Gradient(pimble_board::Gradient::default_linear(Color::rgb(255, 0, 0))))?;
            Ok(id)
        })
        .unwrap();
        tools::select(store, P[0], vec![rect.clone()]);
        let st = store.board_style;
        st.front.set(rail::Well::Fill);
        store.pane(P[0]).board_ui.popup.set(Popup::Well);
        store.board_tool.set(Tool::Select);
        // The end handle sits at the box's right middle; dragged to its
        // bottom right.
        mouse_drag(store, on_screen(Point::new(200.0, 50.0)), on_screen(Point::new(200.0, 100.0)));
        let (g, bounds) = with_session(P[0], |s| {
            let e = s.snapshot().get(&rect).unwrap();
            (e.style.fill_gradient.clone().unwrap(), e.bounds)
        })
        .unwrap();
        assert!((g.to.x - 1.0).abs() < 0.03 && (g.to.y - 1.0).abs() < 0.03, "{:?}", g.to);
        assert_eq!(g.from, Point::new(0.0, 0.5));
        assert!(bounds.x < 1.0, "the rectangle did not move: {bounds:?}");
        tools::undo(store, P[0], false);
        let back = with_session(P[0], |s| s.snapshot().get(&rect).unwrap().style.fill_gradient.clone().unwrap()).unwrap();
        assert_eq!(back.to, Point::new(1.0, 0.5), "one undo step");
        stop(store, P[0]);
    }

    /// Flipping a selection mirrors it through its transform.
    #[test]
    fn flipping_mirrors_the_selection() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        let star = local_edit(store, P[0], |t| t.add_element(NewElement::new(Kind::Star, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 100.0))))).unwrap();
        tools::select(store, P[0], vec![star.clone()]);
        tools::flip(store, P[0], pimble_board::FlipAxis::Vertical);
        let t = with_session(P[0], |s| s.snapshot().get(&star).unwrap().raw.transform).unwrap();
        assert!(t.sy < 0.0 || t.sx < 0.0 || t.rot.abs() > 1.0, "{t:?}");
        tools::flip(store, P[0], pimble_board::FlipAxis::Vertical);
        let t = with_session(P[0], |s| s.snapshot().get(&star).unwrap().raw.transform).unwrap();
        assert!(t.tx.abs() < 1e-3 && t.ty.abs() < 1e-3 && t.rot.abs() < 1e-3 && (t.sx - 1.0).abs() < 1e-4 && (t.sy - 1.0).abs() < 1e-4, "flipped twice: {t:?}");
        assert_eq!(tools::flip_key("H"), Some(pimble_board::FlipAxis::Horizontal));
        assert_eq!(tools::flip_key("v"), Some(pimble_board::FlipAxis::Vertical));
        assert_eq!(tools::flip_key("x"), None);
        stop(store, P[0]);
    }

    /// SVG in: pasted markup lands centered in the view, its group as a
    /// group, selected, in a new "Vector 1" when the board has no vector
    /// layer, in one undo step; what was left out is said.
    #[test]
    fn svg_markup_is_drawn_in() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(800, 600));
        let vector = with_session(P[0], |s| s.snapshot().layers[0].id.clone()).unwrap();
        local_edit(store, P[0], |t| {
            t.add_layer_in("Paint 1", pimble_board::LayerKind::Paint, None, Placement::Top)?;
            t.delete_layer(&vector)
        })
        .unwrap();
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
            <defs><linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>
            <mask id="m"><rect width="10" height="10" fill="white"/></mask></defs>
            <g transform="translate(10 10)"><rect width="40" height="40" fill="url(#g)"/><circle cx="60" cy="60" r="20" fill="green"/></g>
            <path d="M0 90 L100 90" stroke="black" mask="url(#m)"/>
        </svg>"##;
        assert!(svg_import::is_svg_markup(svg));
        svg_import::import_text(store, P[0], svg, None);
        let (count, selected, layer, center) = with_session(P[0], |s| {
            let snap = s.snapshot();
            let top: Vec<_> = s.selected.clone();
            let layer = snap.layer(&snap.get(&top[0]).unwrap().layer).unwrap().name.clone();
            (snap.elements.len(), top, layer, s.selection_bounds().unwrap().center())
        })
        .unwrap();
        assert_eq!(count, 4, "a group of two and a path");
        assert_eq!(selected.len(), 2, "the group and the path");
        assert_eq!(layer, "Vector 1");
        let view = with_session(P[0], |s| s.view.center()).unwrap();
        assert!((center.x - view.x).abs() < 15.0 && (center.y - view.y).abs() < 15.0, "{center:?} {view:?}");
        assert!(untracked(|| store.notice.get()).contains("skipped: masks"), "{}", untracked(|| store.notice.get()));
        assert_eq!(untracked(|| store.board_tool.get()), Tool::Select);
        tools::undo(store, P[0], false);
        assert_eq!(element_count(P[0]), 0, "one undo step");
        stop(store, P[0]);
    }

    // ── Wave 5 (docs/DRAWING_CONTRACT.md, "Built in wave 5 (UI)") ──────────
    use pimble_board::PaintBody;

    /// The paint operations on pane 0's board: their kinds, in drawing order.
    fn op_kinds() -> Vec<String> {
        with_session(P[0], |s| s.snapshot().paint.iter().map(|op| op.raw.body.kind().to_string()).collect()).unwrap()
    }

    /// A closed ring of brush strokes around board point `c`.
    fn paint_ring(store: AppStore, c: Point, r: f32) {
        use rinch::render_surface::SurfacePointer;
        let at = |i: usize| {
            let a = i as f32 / 48.0 * std::f32::consts::TAU;
            on_screen(Point::new(c.x + r * a.cos(), c.y + r * a.sin()))
        };
        let (x, y) = at(0);
        tools::pointer_down(store, P[0], x, y, SurfaceMouseButton::Left, SurfacePointer::mouse(true));
        for i in 1..=50 {
            let (x, y) = at(i);
            tools::pointer_move(store, P[0], x, y, SurfacePointer::mouse(true));
        }
        let (x, y) = at(50);
        tools::pointer_up(store, P[0], x, y, SurfaceMouseButton::Left, SurfacePointer::mouse(false));
    }

    fn color_at(p: Point) -> Color {
        with_session(P[0], |s| {
            let (pictures, _) = pictures_of(s.snapshot());
            pimble_board::sample_color(s.snapshot(), &pictures, p, None)
        })
        .unwrap()
    }

    /// The Fill (G) pours the well's front square inside a painted ring, in
    /// the brush's layer, clipped to what is seen with the layer's painting,
    /// at the context bar's tolerance; one undo step.
    #[test]
    fn the_fill_pours_inside_a_ring_in_one_undo_step() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        let st = store.board_style;
        st.stroke.set("#101010".to_string());
        store.board_tool.set(Tool::Brush);
        let c = Point::new(0.0, 0.0);
        paint_ring(store, c, 80.0);
        assert_eq!(op_kinds(), vec!["stroke"]);
        let layer = untracked(|| store.pane(P[0]).board_ui.layer.get()).unwrap();

        st.stroke.set("#20a040".to_string());
        st.tolerance.set(0.3);
        store.board_tool.set(Tool::Fill);
        mouse_click(store, on_screen(c));
        assert_eq!(op_kinds(), vec!["stroke", "fill"]);
        let fill = with_session(P[0], |s| match &s.snapshot().paint[1].raw.body {
            PaintBody::Fill(f) => (s.snapshot().paint[1].raw.layer.clone(), f.clone()),
            _ => unreachable!(),
        })
        .unwrap();
        assert_eq!(fill.0, layer, "into the brush's layer");
        assert_eq!(fill.1.color.to_hex(), "#20a040");
        assert!((fill.1.tolerance - 0.3).abs() < 1e-6 && fill.1.contiguous);
        assert!(fill.1.clip.contains(c) && fill.1.clip.contains(Point::new(80.0, 0.0)), "the ring's painting is in the clip: {:?}", fill.1.clip);
        let inside = color_at(Point::new(10.0, 10.0));
        assert!(inside.g > 140 && inside.r < 60 && inside.a > 250, "filled inside: {inside:?}");
        let outside = color_at(Point::new(120.0, 0.0));
        assert!(outside.g < 100 || outside.a < 10, "not outside the ring: {outside:?}");

        // Everywhere: a second fill is not contiguous.
        st.contiguous.set(false);
        mouse_click(store, on_screen(Point::new(120.0, 0.0)));
        let everywhere = with_session(P[0], |s| match &s.snapshot().paint.last().unwrap().raw.body {
            PaintBody::Fill(f) => f.contiguous,
            _ => true,
        })
        .unwrap();
        assert!(!everywhere);

        tools::undo(store, P[0], false);
        tools::undo(store, P[0], false);
        assert_eq!(op_kinds(), vec!["stroke"], "each fill is one undo step");
        stop(store, P[0]);
    }

    /// A fill with no paint layer makes "Paint 1" with it, and one undo
    /// takes both back.
    #[test]
    fn a_fill_makes_its_paint_layer_in_the_same_undo_step() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        let layers_before = with_session(P[0], |s| s.snapshot().layers.len()).unwrap();
        store.board_tool.set(Tool::Fill);
        mouse_click(store, on_screen(Point::new(0.0, 0.0)));
        assert_eq!(op_kinds(), vec!["fill"]);
        assert_eq!(with_session(P[0], |s| s.snapshot().layers.len()).unwrap(), layers_before + 1);
        tools::undo(store, P[0], false);
        assert!(op_kinds().is_empty());
        assert_eq!(with_session(P[0], |s| s.snapshot().layers.len()).unwrap(), layers_before);
        stop(store, P[0]);
    }

    /// Smudge (R) and Blur (Shift+R) stream on the paint under them as a
    /// stroke does, with their own size and the strength; nowhere painted
    /// they say so and write nothing.
    #[test]
    fn smudge_and_blur_work_the_paint_under_them() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        store.board_tool.set(Tool::Brush);
        mouse_drag(store, on_screen(Point::new(-100.0, 0.0)), on_screen(Point::new(100.0, 0.0)));
        store.pane(P[0]).board_ui.layer.set(None);

        let key = |k: &str, shift: bool| rinch::render_surface::SurfaceKeyData { key: k.into(), code: String::new(), ctrl: false, shift, alt: false, meta: false };
        tools::on_event(store, P[0], SurfaceEvent::KeyDown(key("r", false)));
        assert_eq!(untracked(|| store.board_tool.get()), Tool::Smudge);
        store.board_style.strength.set(0.75);
        paint::step_size(store, P[0], true);
        let size = untracked(|| store.board_style.retouch_size.get());
        assert!(size > paint::RETOUCH_SIZE, "] makes Smudge larger");
        mouse_drag(store, on_screen(Point::new(0.0, -20.0)), on_screen(Point::new(0.0, 40.0)));
        tools::on_event(store, P[0], SurfaceEvent::KeyDown(key("R", true)));
        assert_eq!(untracked(|| store.board_tool.get()), Tool::Blur);
        mouse_drag(store, on_screen(Point::new(-50.0, 0.0)), on_screen(Point::new(50.0, 0.0)));
        let retouches: Vec<(pimble_board::Retouch, f32, f32, usize, String)> = with_session(P[0], |s| {
            s.snapshot()
                .paint
                .iter()
                .filter_map(|op| match &op.raw.body {
                    PaintBody::Retouch(r) => Some((r.effect, r.strength, r.settings.size, r.points.len(), op.raw.layer.clone())),
                    _ => None,
                })
                .collect()
        })
        .unwrap();
        assert_eq!(retouches.len(), 2);
        assert_eq!(retouches[0].0, pimble_board::Retouch::Smudge);
        assert_eq!(retouches[1].0, pimble_board::Retouch::Blur);
        assert!((retouches[0].1 - 0.75).abs() < 1e-6);
        assert_eq!(retouches[0].2, size);
        assert!(retouches[0].3 >= 5, "streamed: {}", retouches[0].3);
        let stroke_layer = strokes()[0].0.clone();
        assert!(retouches.iter().all(|r| r.4 == stroke_layer), "on the layer painted there");

        // Away from the painting, the layer with painting still takes it.
        mouse_drag(store, on_screen(Point::new(0.0, 500.0)), on_screen(Point::new(50.0, 500.0)));
        assert_eq!(op_kinds().len(), 4);
        tools::undo(store, P[0], false);

        // One undo step each.
        tools::undo(store, P[0], false);
        tools::undo(store, P[0], false);
        assert_eq!(op_kinds(), vec!["stroke"]);
        stop(store, P[0]);
    }

    fn moves() -> Vec<pimble_board::MoveOp> {
        with_session(P[0], |s| {
            s.snapshot()
                .paint
                .iter()
                .filter_map(|op| match &op.raw.body {
                    PaintBody::Move(m) => Some(m.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap()
    }

    fn key_event(store: AppStore, k: &str) {
        let key = rinch::render_surface::SurfaceKeyData { key: k.into(), code: String::new(), ctrl: false, shift: false, alt: false, meta: false };
        tools::on_event(store, P[0], SurfaceEvent::KeyDown(key));
    }

    /// Rectangle select (M) over paint: a drag inside moves the pixels (a
    /// move written on the first drag, changed while dragging), a handle
    /// scales them; Enter keeps them and the whole lift and place is one
    /// undo step. Escape takes a selection back; a lasso (L) and Delete
    /// clear the pixels.
    #[test]
    fn paint_selections_move_scale_and_clear_pixels() {
        let (store, _commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        store.board_tool.set(Tool::Brush);
        mouse_drag(store, on_screen(Point::new(-100.0, 0.0)), on_screen(Point::new(100.0, 0.0)));

        key_event(store, "m");
        assert_eq!(untracked(|| store.board_tool.get()), Tool::SelectRect);
        mouse_drag(store, on_screen(Point::new(-50.0, -30.0)), on_screen(Point::new(50.0, 30.0)));
        let sel = with_session(P[0], |s| s.paint_sel.clone()).unwrap().expect("a selection");
        assert_eq!(sel.selection.bounds().map(|b| (b.w.round(), b.h.round())), Some((100.0, 60.0)));
        assert!(moves().is_empty(), "nothing written until it is dragged");
        assert!(untracked(|| store.pane(P[0]).board_ui.handles.get()).is_some(), "the handles show");
        assert!(with_session(P[0], |s| paint_select::overlay(s).is_some_and(|o| o.dashed.len() == 1)).unwrap(), "the ants march");

        // Dragged inside: moved 40 down, twice (one move operation).
        mouse_drag(store, on_screen(Point::new(0.0, 0.0)), on_screen(Point::new(0.0, 20.0)));
        mouse_drag(store, on_screen(Point::new(0.0, 20.0)), on_screen(Point::new(0.0, 40.0)));
        let all = moves();
        assert_eq!(all.len(), 1);
        let t = all[0].transform.unwrap();
        assert!((t.f - 40.0).abs() < 0.01 && t.e.abs() < 0.01, "{t:?}");
        // The bottom right handle: twice as wide and tall.
        let b = with_session(P[0], |s| s.paint_sel.as_ref().unwrap().box_now().unwrap()).unwrap();
        mouse_drag(store, on_screen(Point::new(b.right(), b.bottom())), on_screen(Point::new(b.x + b.w * 2.0, b.y + b.h * 2.0)));
        let t = moves()[0].transform.unwrap();
        assert!((t.a - 2.0).abs() < 0.01 && (t.d - 2.0).abs() < 0.01, "{t:?}");
        key_event(store, "Enter");
        assert!(with_session(P[0], |s| s.paint_sel.is_none()).unwrap());
        assert_eq!(moves().len(), 1, "kept");
        tools::undo(store, P[0], false);
        assert!(moves().is_empty(), "the whole lift and place is one undo step");
        assert_eq!(op_kinds(), vec!["stroke"]);

        // Escape after a drag: the move goes.
        mouse_drag(store, on_screen(Point::new(-50.0, -30.0)), on_screen(Point::new(50.0, 30.0)));
        mouse_drag(store, on_screen(Point::new(0.0, 0.0)), on_screen(Point::new(30.0, 0.0)));
        assert_eq!(moves().len(), 1);
        key_event(store, "Escape");
        assert!(moves().is_empty() && with_session(P[0], |s| s.paint_sel.is_none()).unwrap());

        // A lasso, and Delete: a move that puts nothing down.
        key_event(store, "l");
        assert_eq!(untracked(|| store.board_tool.get()), Tool::SelectLasso);
        {
            use rinch::render_surface::SurfacePointer;
            let pts = [(-40.0, -20.0), (40.0, -20.0), (40.0, 20.0), (-40.0, 20.0)];
            let (x, y) = on_screen(Point::new(pts[0].0, pts[0].1));
            tools::pointer_down(store, P[0], x, y, SurfaceMouseButton::Left, SurfacePointer::mouse(true));
            for &(bx, by) in &pts[1..] {
                let (x, y) = on_screen(Point::new(bx, by));
                tools::pointer_move(store, P[0], x, y, SurfacePointer::mouse(true));
            }
            tools::pointer_up(store, P[0], x - 0.1, y, SurfaceMouseButton::Left, SurfacePointer::mouse(false));
        }
        assert!(matches!(with_session(P[0], |s| s.paint_sel.clone()).unwrap().map(|s| s.selection), Some(pimble_board::Selection::Lasso(_))));
        key_event(store, "Delete");
        let all = moves();
        assert_eq!(all.len(), 1);
        assert!(all[0].transform.is_none(), "cleared");
        let gone = color_at(Point::new(0.0, 0.0));
        assert!(gone.a < 10, "the pixels inside are gone: {gone:?}");
        let kept = color_at(Point::new(80.0, 0.0));
        assert!(kept.a > 200, "the ones outside stay: {kept:?}");
        tools::undo(store, P[0], false);
        assert!(moves().is_empty());

        // Another tool keeps a selection's pixels where they are.
        key_event(store, "m");
        mouse_drag(store, on_screen(Point::new(-50.0, -30.0)), on_screen(Point::new(50.0, 30.0)));
        mouse_drag(store, on_screen(Point::new(0.0, 0.0)), on_screen(Point::new(10.0, 0.0)));
        store.board_tool.set(Tool::Brush);
        points::tool_changed(store, P[0]);
        assert!(with_session(P[0], |s| s.paint_sel.is_none()).unwrap());
        assert_eq!(moves().len(), 1);

        stop(store, P[0]);

        // A board with no painting: a sentence, and no selection.
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        store.board_tool.set(Tool::SelectRect);
        mouse_drag(store, on_screen(Point::new(0.0, 400.0)), on_screen(Point::new(50.0, 450.0)));
        assert!(with_session(P[0], |s| s.paint_sel.is_none()).unwrap());
        stop(store, P[0]);
    }

    /// The next `PutBlob` of a flattened layer, answered as stored at its
    /// own size.
    fn answer_raster(store: AppStore, commands: &crossbeam_channel::Receiver<BackendCommand>) -> (u32, u32) {
        let (request_id, bytes) = commands
            .try_iter()
            .find_map(|c| match c {
                BackendCommand::PutBlob { request_id, bytes, .. } => Some((request_id, bytes)),
                _ => None,
            })
            .expect("the picture is stored");
        assert!(request_id >= 1 << 41, "a raster's request: {request_id}");
        let size = Picture::decode(&bytes).map(|p| (p.width(), p.height())).unwrap();
        let url = BlobUrl::parse(&format!("pimble-blob:00000000-0000-0000-0000-000000000001/{}a", "c".repeat(25))).unwrap();
        assert!(stored(store, request_id, &Ok(StoredBlob { url, size: Some(size), notice: None })));
        size
    }

    /// Flatten Layer: drawn, stored, laid down as one `pixels` operation
    /// that looks the same, in one undo step; the panel says so meanwhile.
    /// Merge Down of a vector layer onto a paint layer goes the same way.
    #[test]
    fn flatten_and_merge_down_go_through_the_blob_flow() {
        let (store, commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        store.board_style.stroke.set("#c02020".to_string());
        store.board_tool.set(Tool::Brush);
        mouse_drag(store, on_screen(Point::new(-100.0, 0.0)), on_screen(Point::new(100.0, 0.0)));
        mouse_drag(store, on_screen(Point::new(0.0, -60.0)), on_screen(Point::new(0.0, 60.0)));
        assert_eq!(op_kinds(), vec!["stroke", "stroke"]);
        let layer = untracked(|| store.pane(P[0]).board_ui.layer.get()).unwrap();
        let before = color_at(Point::new(50.0, 0.0));
        let _ = commands.try_iter().count();

        layers::rasterize_layer(store, P[0], pimble_board::RasterJob::Flatten(layer.clone()));
        assert_eq!(untracked(|| store.pane(P[0]).board_ui.rasterizing.get()), Some("Flattening..."));
        assert_eq!(op_kinds(), vec!["stroke", "stroke"], "nothing changes until the picture is stored");
        answer_raster(store, &commands);
        assert_eq!(untracked(|| store.pane(P[0]).board_ui.rasterizing.get()), None);
        assert_eq!(op_kinds(), vec!["pixels"]);
        let after = color_at(Point::new(50.0, 0.0));
        assert!(before.r.abs_diff(after.r) <= 4 && before.a.abs_diff(after.a) <= 4, "looks the same: {before:?} {after:?}");
        tools::undo(store, P[0], false);
        assert_eq!(op_kinds(), vec!["stroke", "stroke"], "one undo step");

        // A vector layer above the paint layer, merged down onto it.
        let rect = local_edit(store, P[0], |t| {
            let vector = t.add_layer("Shapes", Placement::Above(layer.clone()))?;
            t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(150.0, -20.0, 40.0, 40.0))).with_style(Style { fill: Color::parse("#2040c0"), stroke: Color::parse("#2040c0"), ..Style::default() }).in_layer(vector.clone()))?;
            Ok(vector)
        })
        .unwrap();
        let rows = untracked(|| store.pane(P[0]).board_ui.layers.get());
        let row = rows.iter().find(|r| r.id == rect).unwrap().clone();
        layers::merge(store, P[0], &row);
        answer_raster(store, &commands);
        let layers_now: Vec<String> = with_session(P[0], |s| s.snapshot().layers.iter().map(|l| l.id.clone()).collect()).unwrap();
        assert!(!layers_now.contains(&rect), "the vector layer went");
        assert_eq!(op_kinds(), vec!["stroke", "stroke", "pixels"]);
        let blue = color_at(Point::new(170.0, 0.0));
        assert!(blue.b > 150 && blue.r < 80, "the rectangle is pixels now: {blue:?}");
        assert_eq!(untracked(|| store.pane(P[0]).board_ui.layer.get()), Some(layer.clone()), "the layer it went into is picked");
        stop(store, P[0]);
    }

    /// A paint layer wider than one tile (docs/DRAWING_CONTRACT.md "Large
    /// canvases"): Flatten stores a picture per tile and lays them all down
    /// in one edit once the last is stored (one undo step); nothing changes
    /// before, and a tile that fails to store writes nothing.
    #[test]
    fn a_wide_layer_flattens_in_tiles_in_one_edit() {
        let (store, commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        let layer = local_edit(store, P[0], |t| {
            let paint = t.paint_layer_for(None)?;
            let points: Vec<pimble_board::BrushPoint> = (0..120).map(|i| [-1500.0 + i as f32 * 25.0, 0.0, 1.0, 0.0]).collect();
            t.add_stroke(pimble_board::NewStroke::new(&paint, &pimble_board::brush::ROUND, Color::rgb(200, 30, 30)).with_size(10.0).with_points(points))?;
            Ok(paint)
        })
        .unwrap();
        let _ = commands.try_iter().count();
        let blobs = |commands: &crossbeam_channel::Receiver<BackendCommand>| -> Vec<u64> {
            commands.try_iter().filter_map(|c| match c {
                BackendCommand::PutBlob { request_id, .. } => Some(request_id),
                _ => None,
            }).collect()
        };
        let url = |i: u64| {
            let blob = pimble_core::BlobId::from_bytes([i as u8 + 1; 16]);
            BlobUrl::parse(&format!("pimble-blob:00000000-0000-0000-0000-000000000001/{blob}")).unwrap()
        };
        // A tile that fails: nothing written, the panel free again.
        layers::rasterize_layer(store, P[0], pimble_board::RasterJob::Flatten(layer.clone()));
        let ids = blobs(&commands);
        assert!(ids.len() >= 3, "a picture per tile: {}", ids.len());
        assert!(stored(store, ids[0], &Ok(StoredBlob { url: url(0), size: None, notice: None })));
        assert!(stored(store, ids[1], &Err("The disk is full.".into())));
        assert_eq!(untracked(|| store.pane(P[0]).board_ui.rasterizing.get()), None);
        assert!(!stored(store, ids[2], &Ok(StoredBlob { url: url(2), size: None, notice: None })), "the job is gone");
        assert_eq!(op_kinds(), vec!["stroke"]);
        // Every tile stored: one edit, one undo.
        layers::rasterize_layer(store, P[0], pimble_board::RasterJob::Flatten(layer.clone()));
        let ids = blobs(&commands);
        for (k, id) in ids.iter().enumerate() {
            assert_eq!(op_kinds(), vec!["stroke"], "nothing changes until the last picture is stored");
            assert!(stored(store, *id, &Ok(StoredBlob { url: url(k as u64), size: None, notice: None })));
        }
        assert_eq!(op_kinds(), vec!["pixels"; ids.len()]);
        tools::undo(store, P[0], false);
        assert_eq!(op_kinds(), vec!["stroke"], "one undo step");
        stop(store, P[0]);
    }

    // ── Wave 6: pictures in paint layers (docs/DRAWING_WAVE6_CONTRACT.md) ───

    /// A PNG `w` × `h` of one color.
    fn png_of(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        let pixels: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
        ::image::ImageEncoder::write_image(::image::codecs::png::PngEncoder::new(&mut out), &pixels, w, h, ::image::ExtendedColorType::Rgba8).unwrap();
        out
    }

    /// Every `PutBlob` sent since the last look: request id and bytes.
    fn puts(commands: &crossbeam_channel::Receiver<BackendCommand>) -> Vec<(u64, Vec<u8>)> {
        commands
            .try_iter()
            .filter_map(|c| match c {
                BackendCommand::PutBlob { request_id, bytes, .. } => Some((request_id, bytes)),
                _ => None,
            })
            .collect()
    }

    /// Each `PutBlob` answered as stored at the size its bytes decode to.
    fn answer_puts(store: AppStore, sent: &[(u64, Vec<u8>)]) {
        for (k, (request_id, bytes)) in sent.iter().enumerate() {
            let size = Picture::decode(bytes).map(|p| (p.width(), p.height())).ok();
            let blob = pimble_core::BlobId::from_bytes([k as u8 + 7; 16]);
            let url = BlobUrl::parse(&format!("pimble-blob:00000000-0000-0000-0000-000000000001/{blob}")).unwrap();
            assert!(stored(store, *request_id, &Ok(StoredBlob { url, size, notice: None })));
        }
    }

    fn pixels_ops() -> Vec<(String, pimble_board::PixelsOp)> {
        with_session(P[0], |s| {
            s.snapshot()
                .paint
                .iter()
                .filter_map(|op| match &op.raw.body {
                    PaintBody::Pixels(p) => Some((op.raw.layer.clone(), p.clone())),
                    _ => None,
                })
                .collect()
        })
        .unwrap()
    }

    /// A picture pasted with a paint tool in hand is placed, not added: it
    /// shows with the selection's handles, fitted to the view, and nothing
    /// is written or stored while it is moved and scaled. Enter stores it
    /// and lays it down as one `pixels` operation with its rectangle and
    /// transform, in a paint layer made in the same edit: one undo step.
    /// Painting goes on above it in the same layer.
    #[test]
    fn a_picture_pasted_into_a_paint_layer_is_placed_then_laid_down() {
        let (store, commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(1600, 1200));
        store.board_tool.set(Tool::Brush);
        let layers_before = with_session(P[0], |s| s.snapshot().layers.len()).unwrap();
        let _ = commands.try_iter().count();

        add_to_board(store, P[0], None, vec![(None, png_of(4000, 1000, [30, 60, 220, 255]))]);
        assert!(paint_select::placing(P[0]), "placed first");
        assert!(puts(&commands).is_empty() && op_kinds().is_empty(), "nothing stored or written while placing");
        assert!(pictures_on(P[0]).is_empty(), "not a picture element");
        let placed = with_session(P[0], |s| s.paint_sel.as_ref().unwrap().box_now().unwrap()).unwrap();
        let view = with_session(P[0], |s| s.view.size()).unwrap();
        assert!(placed.w < view.0 as f32 && (placed.w / placed.h - 4.0).abs() < 0.01, "fitted to the view, its proportions kept: {placed:?}");
        assert!(untracked(|| store.pane(P[0]).board_ui.handles.get()).is_some(), "the handles show");
        assert!(with_session(P[0], |s| paint_select::overlay(s).is_some_and(|o| o.picture.is_some())).unwrap(), "drawn over the canvas");

        // Moved by a drag inside, made twice as tall by the bottom handle.
        let c = placed.center();
        mouse_drag(store, on_screen(c), on_screen(Point::new(c.x + 40.0, c.y)));
        let b = with_session(P[0], |s| s.paint_sel.as_ref().unwrap().box_now().unwrap()).unwrap();
        mouse_drag(store, on_screen(Point::new(b.center().x, b.bottom())), on_screen(Point::new(b.center().x, b.bottom() + b.h)));
        assert!(puts(&commands).is_empty() && op_kinds().is_empty(), "still nothing written");
        let (rect, t) = with_session(P[0], |s| {
            let sel = s.paint_sel.as_ref().unwrap();
            (sel.selection.bounds().unwrap(), sel.transform)
        })
        .unwrap();
        assert!((t.d - 2.0).abs() < 0.01 && (t.a - 1.0).abs() < 0.01, "{t:?}");

        key_event(store, "Enter");
        assert!(!paint_select::placing(P[0]));
        assert_eq!(untracked(|| store.pane(P[0]).board_ui.rasterizing.get()), Some("Placing the picture..."));
        let sent = puts(&commands);
        assert_eq!(sent.len(), 2, "a picture past 2,048 pixels goes in pieces");
        assert!(sent.iter().all(|(id, _)| *id >= 1 << 42), "a placement's requests");
        assert!(op_kinds().is_empty(), "nothing until every piece is stored");
        answer_puts(store, &sent);
        assert_eq!(untracked(|| store.pane(P[0]).board_ui.rasterizing.get()), None);
        let ops = pixels_ops();
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|(_, p)| p.transform == Some(t)), "every piece goes through the placement's transform");
        let union = ops[0].1.rect.union(&ops[1].1.rect);
        assert!((union.x - rect.x).abs() < 0.01 && (union.w - rect.w).abs() < 0.01 && (union.h - rect.h).abs() < 0.01, "{union:?} {rect:?}");
        assert!((ops[0].1.rect.right() - ops[1].1.rect.x).abs() < 1e-3, "the pieces meet");
        let layer = ops[0].0.clone();
        assert_eq!(with_session(P[0], |s| s.snapshot().layers.len()).unwrap(), layers_before + 1, "a paint layer made for it");
        assert_eq!(untracked(|| store.pane(P[0]).board_ui.layer.get()), Some(layer.clone()), "and picked");

        // Painted over, in the same layer.
        mouse_drag(store, on_screen(Point::new(c.x - 100.0, c.y)), on_screen(Point::new(c.x + 100.0, c.y)));
        assert_eq!(op_kinds(), vec!["pixels", "pixels", "stroke"]);
        assert!(with_session(P[0], |s| s.snapshot().paint.iter().all(|op| op.raw.layer == layer)).unwrap());
        tools::undo(store, P[0], false);
        assert_eq!(op_kinds(), vec!["pixels", "pixels"]);
        tools::undo(store, P[0], false);
        assert!(op_kinds().is_empty(), "the picture and its layer are one undo step");
        assert_eq!(with_session(P[0], |s| s.snapshot().layers.len()).unwrap(), layers_before);
        stop(store, P[0]);
    }

    /// A picture dropped with a paint layer picked goes into that layer
    /// and shows at once (its picture seeded, one piece); turned with the
    /// rotate handle it keeps the turn. Escape (and undo) drops a placement
    /// with nothing stored or written; a press outside lays it down.
    #[test]
    fn a_placement_is_turned_kept_by_a_press_outside_and_escape_drops_it() {
        let (store, commands) = store_with_commands();
        start(store, P[0], StoreId::new(), NodeId::new(), &whiteboard_bytes());
        with_session(P[0], |s| s.view.resize(1600, 1200));
        let paint = local_edit(store, P[0], |t| t.paint_layer_for(None)).unwrap();
        store.pane(P[0]).board_ui.layer.set(Some(paint.clone()));
        store.board_tool.set(Tool::Select);
        let _ = commands.try_iter().count();
        let png = png_of(200, 100, [220, 40, 40, 255]);

        // Escape: nothing.
        add_to_board(store, P[0], None, vec![(None, png.clone())]);
        assert!(paint_select::placing(P[0]));
        key_event(store, "Escape");
        assert!(!paint_select::placing(P[0]));
        assert!(puts(&commands).is_empty() && op_kinds().is_empty(), "Escape adds nothing");
        // Undo drops one too, and that is all it does.
        add_to_board(store, P[0], None, vec![(None, png.clone())]);
        tools::undo(store, P[0], false);
        assert!(!paint_select::placing(P[0]) && puts(&commands).is_empty());
        assert_eq!(with_session(P[0], |s| s.snapshot().layer(&paint).is_some()).unwrap(), true);

        // Dropped at a point, turned a quarter, laid down by a press outside.
        let at = on_screen(Point::new(300.0, 200.0));
        add_to_board(store, P[0], Some(at), vec![(Some("red.png".into()), png.clone())]);
        let b = with_session(P[0], |s| s.paint_sel.as_ref().unwrap().box_now().unwrap()).unwrap();
        assert!((b.center().x - 300.0).abs() < 0.5 && (b.center().y - 200.0).abs() < 0.5, "where it was dropped: {b:?}");
        assert_eq!((b.w, b.h), (200.0, 100.0), "one unit per pixel when it fits");
        let center = b.center();
        with_session(P[0], |s| {
            let sel = s.paint_sel.as_mut().unwrap();
            sel.transform = pimble_board::PixelTransform::rotate(90.0, center);
        });
        mouse_click(store, on_screen(Point::new(center.x + 400.0, center.y + 400.0)));
        assert!(!paint_select::placing(P[0]));
        let sent = puts(&commands);
        assert_eq!(sent.len(), 1, "one piece");
        assert_eq!(sent[0].1, png, "stored as it came");
        answer_puts(store, &sent);
        let ops = pixels_ops();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].0, paint, "into the picked paint layer");
        assert_eq!(ops[0].1.rect, b);
        let bounds = with_session(P[0], |s| s.snapshot().paint[0].bounds).unwrap().unwrap();
        assert!((bounds.w - 100.0).abs() < 0.01 && (bounds.h - 200.0).abs() < 0.01, "turned: {bounds:?}");
        let red = color_at(Point::new(center.x, center.y + 80.0));
        assert!(red.r > 200 && red.g < 80 && red.a > 200, "it shows at once, turned: {red:?}");
        let empty = color_at(Point::new(center.x + 80.0, center.y));
        assert!(empty.a < 10, "not where it was before the turn: {empty:?}");
        tools::undo(store, P[0], false);
        assert!(op_kinds().is_empty(), "one undo step");
        stop(store, P[0]);
    }
}
