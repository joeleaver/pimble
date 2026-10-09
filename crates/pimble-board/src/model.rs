//! The board document's values as Rust types (docs/IMAGE_MARKUP_CONTRACT.md
//! section 3). Everything here is plain data: [`crate::doc`] writes it into
//! yrs and reads it back, [`crate::snapshot`] resolves it for drawing.
//!
//! Units: every coordinate, width and font size is in board units (contract
//! "Board units"). Rotation is in degrees, clockwise (the y axis points down).
//! Timestamps are the caller's RFC 3339 strings, as `NodeDoc` stamps them;
//! this crate never reads a clock.

use std::fmt;

/// A point in board units.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle in board units (`w`, `h` never negative once
/// normalized).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// The rectangle spanning two corners, in any order.
    pub fn from_points(a: Point, b: Point) -> Self {
        Self { x: a.x.min(b.x), y: a.y.min(b.y), w: (a.x - b.x).abs(), h: (a.y - b.y).abs() }
    }

    /// Negative sizes flipped into positive ones.
    pub fn normalized(self) -> Self {
        Self::from_points(Point::new(self.x, self.y), Point::new(self.x + self.w, self.y + self.h))
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect::new(x, y, self.right().max(other.right()) - x, self.bottom().max(other.bottom()) - y)
    }

    /// The overlap, `None` when the rectangles do not overlap.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        (r > x && b > y).then(|| Rect::new(x, y, r - x, b - y))
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right() && other.x < self.right() && self.y < other.bottom() && other.y < self.bottom()
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.right() && p.y >= self.y && p.y <= self.bottom()
    }

    pub fn inflate(&self, by: f32) -> Rect {
        Rect::new(self.x - by, self.y - by, self.w + 2.0 * by, self.h + 2.0 * by)
    }

    pub fn area(&self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    pub fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.w.is_finite() && self.h.is_finite()
    }
}

/// Who made or changed something (contract section 6): the signed-in Pimble
/// Cloud account's `sub` and a name stamped at the time. Absent when nobody
/// is signed in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Author {
    pub id: String,
    pub name: String,
}

impl Author {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self { id: id.into(), name: name.into() }
    }
}

/// What every write is stamped with: the author when someone is signed in,
/// and the caller's clock.
#[derive(Debug, Clone, Copy)]
pub struct Stamp<'a> {
    pub author: Option<&'a Author>,
    /// RFC 3339, UTC (`2026-10-09T12:00:00.000Z`): step numbers sort by it
    /// as a string, so every writer must use the same shape.
    pub now: &'a str,
}

impl<'a> Stamp<'a> {
    pub fn new(author: Option<&'a Author>, now: &'a str) -> Self {
        Self { author, now }
    }
}

/// An element's kind (contract section 3). A kind this version does not know
/// is kept as `Other` and never drawn.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Kind {
    Picture,
    Pen,
    Highlighter,
    Line,
    Arrow,
    Rect,
    Ellipse,
    Diamond,
    Text,
    Sticky,
    Step,
    Callout,
    Connector,
    Frame,
    Group,
    Card,
    Spotlight,
    Other(String),
}

impl Kind {
    pub fn as_str(&self) -> &str {
        match self {
            Kind::Picture => "picture",
            Kind::Pen => "pen",
            Kind::Highlighter => "highlighter",
            Kind::Line => "line",
            Kind::Arrow => "arrow",
            Kind::Rect => "rect",
            Kind::Ellipse => "ellipse",
            Kind::Diamond => "diamond",
            Kind::Text => "text",
            Kind::Sticky => "sticky",
            Kind::Step => "step",
            Kind::Callout => "callout",
            Kind::Connector => "connector",
            Kind::Frame => "frame",
            Kind::Group => "group",
            Kind::Card => "card",
            Kind::Spotlight => "spotlight",
            Kind::Other(s) => s,
        }
    }

    pub fn parse(s: &str) -> Kind {
        match s {
            "picture" => Kind::Picture,
            "pen" => Kind::Pen,
            "highlighter" => Kind::Highlighter,
            "line" => Kind::Line,
            "arrow" => Kind::Arrow,
            "rect" => Kind::Rect,
            "ellipse" => Kind::Ellipse,
            "diamond" => Kind::Diamond,
            "text" => Kind::Text,
            "sticky" => Kind::Sticky,
            "step" => Kind::Step,
            "callout" => Kind::Callout,
            "connector" => Kind::Connector,
            "frame" => Kind::Frame,
            "group" => Kind::Group,
            "card" => Kind::Card,
            "spotlight" => Kind::Spotlight,
            other => Kind::Other(other.to_string()),
        }
    }

    /// A stroke: its geometry is the appendable point array.
    pub fn is_stroke(&self) -> bool {
        matches!(self, Kind::Pen | Kind::Highlighter)
    }

    /// Whether the markup toggle leaves it showing (contract section 5:
    /// "off hides every element that is not a picture").
    pub fn is_picture(&self) -> bool {
        matches!(self, Kind::Picture)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A picture's geometry: its rectangle on the board, the blob it shows, the
/// blob's pixel size, and an optional crop in the picture's pixels. Rotation
/// is the element's `transform` like any element's (a deviation from the
/// contract's "crop and rotation" in geometry: one rotation, not two).
#[derive(Debug, Clone, PartialEq)]
pub struct PictureGeom {
    pub rect: Rect,
    /// A `pimble-blob:` URL (docs/IMAGES_CONTRACT.md).
    pub src: String,
    pub px_w: u32,
    pub px_h: u32,
    pub crop: Option<Rect>,
}

/// Which side of an element a connector end snaps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Side {
    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Top => "top",
            Side::Right => "right",
            Side::Bottom => "bottom",
            Side::Left => "left",
        }
    }

    pub fn parse(s: &str) -> Option<Side> {
        Some(match s {
            "top" => Side::Top,
            "right" => Side::Right,
            "bottom" => Side::Bottom,
            "left" => Side::Left,
            _ => return None,
        })
    }
}

/// One end of a connector (contract section 3, "Connectors"): an element and
/// optionally its side, routed at render time, with the last point it was
/// seen at, which is where the end stays when the element is deleted; or a
/// free point.
#[derive(Debug, Clone, PartialEq)]
pub enum End {
    Element { id: String, side: Option<Side>, last: Point },
    Point(Point),
}

impl End {
    pub fn last_point(&self) -> Point {
        match self {
            End::Element { last, .. } => *last,
            End::Point(p) => *p,
        }
    }
}

/// An element's geometry, per kind (contract section 3, "Geometry").
#[derive(Debug, Clone, PartialEq)]
pub enum Geom {
    /// A group: its extent is its members'.
    None,
    /// Shapes, sticky, spotlight, frame, and a text box (`w` = 0 means the
    /// text does not wrap; `h` is a minimum, the text grows it).
    Rect(Rect),
    Picture(PictureGeom),
    /// Line and arrow.
    Segment { a: Point, b: Point },
    /// A step badge's center.
    Anchor(Point),
    /// A box with a pointer to `tip`.
    Callout { rect: Rect, tip: Point },
    Connector { from: End, to: End },
    /// A node card: its rectangle and the node's `pimble:` link.
    Card { rect: Rect, link: String },
    /// Pen and highlighter: the points live in the element's appendable
    /// array ([`crate::Board`]'s `append_points`), never in this value.
    Stroke,
}

/// An RGBA color, straight (not premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);

    /// `#rgb`, `#rrggbb` or `#rrggbbaa`.
    pub fn parse(s: &str) -> Option<Color> {
        let hex = s.strip_prefix('#')?;
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        match hex.len() {
            3 => {
                let n = |i: usize| u8::from_str_radix(hex.get(i..i + 1)?, 16).ok().map(|v| v * 17);
                Some(Color::rgb(n(0)?, n(1)?, n(2)?))
            }
            6 => Some(Color::rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Some(Color::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => None,
        }
    }

    /// `#rrggbb`, or `#rrggbbaa` when not opaque.
    pub fn to_hex(&self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }
}

/// Text alignment inside a text element or a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

impl Align {
    pub fn as_str(&self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
        }
    }

    pub fn parse(s: &str) -> Option<Align> {
        Some(match s {
            "left" => Align::Left,
            "center" => Align::Center,
            "right" => Align::Right,
            _ => return None,
        })
    }
}

/// Which ends of a line, arrow or connector carry a head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArrowHeads {
    None,
    Start,
    End,
    Both,
}

impl ArrowHeads {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArrowHeads::None => "none",
            ArrowHeads::Start => "start",
            ArrowHeads::End => "end",
            ArrowHeads::Both => "both",
        }
    }

    pub fn parse(s: &str) -> Option<ArrowHeads> {
        Some(match s {
            "none" => ArrowHeads::None,
            "start" => ArrowHeads::Start,
            "end" => ArrowHeads::End,
            "both" => ArrowHeads::Both,
            _ => return None,
        })
    }

    pub fn start(&self) -> bool {
        matches!(self, ArrowHeads::Start | ArrowHeads::Both)
    }

    pub fn end(&self) -> bool {
        matches!(self, ArrowHeads::End | ArrowHeads::Both)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dash {
    Solid,
    Dashed,
    Dotted,
}

impl Dash {
    pub fn as_str(&self) -> &'static str {
        match self {
            Dash::Solid => "solid",
            Dash::Dashed => "dashed",
            Dash::Dotted => "dotted",
        }
    }

    pub fn parse(s: &str) -> Option<Dash> {
        Some(match s {
            "solid" => Dash::Solid,
            "dashed" => Dash::Dashed,
            "dotted" => Dash::Dotted,
            _ => return None,
        })
    }
}

/// An element's style map: each key its own last-writer-wins value, so one
/// person changing the color while another changes the width keeps both.
/// `None` is "not set": writing a `Style` sets only the keys that are `Some`,
/// and reading leaves unset keys `None` for the renderer's per-kind defaults
/// ([`Style::resolved`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Style {
    pub stroke: Option<Color>,
    pub fill: Option<Color>,
    pub width: Option<f32>,
    pub opacity: Option<f32>,
    pub font_size: Option<f32>,
    pub align: Option<Align>,
    pub shadow: Option<bool>,
    pub arrow_heads: Option<ArrowHeads>,
    pub dash: Option<Dash>,
}

/// A [`Style`] with every key decided, by the element or by its kind's
/// default.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedStyle {
    pub stroke: Color,
    pub fill: Option<Color>,
    pub width: f32,
    pub opacity: f32,
    pub font_size: f32,
    pub align: Align,
    pub shadow: bool,
    pub arrow_heads: ArrowHeads,
    pub dash: Dash,
}

/// The default red of marks (Spectacle's annotation red).
pub const MARK_RED: Color = Color::rgb(0xe5, 0x39, 0x35);
/// The default highlighter yellow.
pub const HIGHLIGHT_YELLOW: Color = Color::rgb(0xff, 0xeb, 0x3b);
/// The default sticky note yellow.
pub const STICKY_YELLOW: Color = Color::rgb(0xff, 0xf5, 0x9d);

impl Style {
    /// Every unset key filled with `kind`'s default.
    pub fn resolved(&self, kind: &Kind) -> ResolvedStyle {
        let (stroke, fill, width, font_size, arrow_heads, align, shadow) = match kind {
            Kind::Pen => (MARK_RED, None, 4.0, 24.0, ArrowHeads::None, Align::Left, false),
            Kind::Highlighter => (HIGHLIGHT_YELLOW, None, 24.0, 24.0, ArrowHeads::None, Align::Left, false),
            Kind::Line => (MARK_RED, None, 4.0, 24.0, ArrowHeads::None, Align::Left, false),
            Kind::Arrow => (MARK_RED, None, 4.0, 24.0, ArrowHeads::End, Align::Left, false),
            Kind::Connector => (Color::rgb(0x42, 0x42, 0x42), None, 2.0, 18.0, ArrowHeads::End, Align::Center, false),
            Kind::Rect | Kind::Ellipse | Kind::Diamond => (MARK_RED, None, 4.0, 24.0, ArrowHeads::None, Align::Center, false),
            Kind::Text => (MARK_RED, None, 0.0, 24.0, ArrowHeads::None, Align::Left, false),
            Kind::Sticky => (Color::rgb(0x21, 0x21, 0x21), Some(STICKY_YELLOW), 0.0, 20.0, ArrowHeads::None, Align::Left, true),
            Kind::Step => (MARK_RED, None, 0.0, 24.0, ArrowHeads::None, Align::Center, false),
            Kind::Callout => (MARK_RED, Some(Color::WHITE), 3.0, 20.0, ArrowHeads::None, Align::Left, false),
            Kind::Frame => (Color::rgb(0x9e, 0x9e, 0x9e), None, 2.0, 18.0, ArrowHeads::None, Align::Left, false),
            Kind::Card => (Color::rgb(0x61, 0x61, 0x61), Some(Color::rgb(0xfa, 0xfa, 0xfa)), 1.5, 18.0, ArrowHeads::None, Align::Left, true),
            Kind::Spotlight => (Color::BLACK, Some(Color::rgba(0, 0, 0, 0x99)), 0.0, 24.0, ArrowHeads::None, Align::Left, false),
            Kind::Picture | Kind::Group | Kind::Other(_) => (Color::BLACK, None, 0.0, 24.0, ArrowHeads::None, Align::Left, false),
        };
        ResolvedStyle {
            stroke: self.stroke.unwrap_or(stroke),
            fill: self.fill.or(fill),
            width: self.width.unwrap_or(width).max(0.0),
            opacity: self.opacity.unwrap_or(1.0).clamp(0.0, 1.0),
            font_size: self.font_size.unwrap_or(font_size).max(1.0),
            align: self.align.unwrap_or(align),
            shadow: self.shadow.unwrap_or(shadow),
            arrow_heads: self.arrow_heads.unwrap_or(arrow_heads),
            dash: self.dash.unwrap_or(Dash::Solid),
        }
    }
}

/// Which style key to clear (see [`crate::BoardTxn::clear_style`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleKey {
    Stroke,
    Fill,
    Width,
    Opacity,
    FontSize,
    Align,
    Shadow,
    ArrowHeads,
    Dash,
}

impl StyleKey {
    pub fn as_str(&self) -> &'static str {
        match self {
            StyleKey::Stroke => "stroke",
            StyleKey::Fill => "fill",
            StyleKey::Width => "width",
            StyleKey::Opacity => "opacity",
            StyleKey::FontSize => "font_size",
            StyleKey::Align => "align",
            StyleKey::Shadow => "shadow",
            StyleKey::ArrowHeads => "arrow_heads",
            StyleKey::Dash => "dash",
        }
    }
}

/// An element's transform: translate by (`tx`, `ty`), then rotate by `rot`
/// degrees clockwise and scale by (`sx`, `sy`), both about the center of the
/// element's own untransformed extent. One value, written whole: two people
/// dragging one element end with one position, never a mix. A member of a
/// frame or group is also moved by its parent's transform (contract section
/// 3: "moving a frame moves what is in it").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub tx: f32,
    pub ty: f32,
    pub rot: f32,
    pub sx: f32,
    pub sy: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Transform = Transform { tx: 0.0, ty: 0.0, rot: 0.0, sx: 1.0, sy: 1.0 };

    pub fn translate(tx: f32, ty: f32) -> Self {
        Transform { tx, ty, ..Self::IDENTITY }
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }
}

/// An optional page (contract section 1, "A page, optionally").
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub rect: Rect,
    pub fill: Option<Color>,
}

/// Where a layer or an element goes in its order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// Above everything (the end of the order).
    Top,
    /// Below everything.
    Bottom,
    /// Just above this sibling.
    Above(String),
    /// Just below this sibling.
    Below(String),
}

/// What [`crate::BoardTxn::add_element`] takes.
#[derive(Debug, Clone)]
pub struct NewElement {
    pub kind: Kind,
    pub geom: Geom,
    /// `None`: the top layer by order.
    pub layer: Option<String>,
    /// `None`: on top of its layer.
    pub placement: Option<Placement>,
    pub parent: Option<String>,
    /// For kinds that carry text (text, sticky, shapes, callout, connector
    /// label, frame name, card).
    pub text: Option<String>,
    pub style: Style,
    pub transform: Option<Transform>,
    /// A pen or highlighter's first points (x, y, pressure).
    pub points: Vec<[f32; 3]>,
}

impl NewElement {
    pub fn new(kind: Kind, geom: Geom) -> Self {
        Self {
            kind,
            geom,
            layer: None,
            placement: None,
            parent: None,
            text: None,
            style: Style::default(),
            transform: None,
            points: Vec::new(),
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn in_layer(mut self, layer: impl Into<String>) -> Self {
        self.layer = Some(layer.into());
        self
    }

    pub fn with_parent(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    pub fn with_points(mut self, points: Vec<[f32; 3]>) -> Self {
        self.points = points;
        self
    }

    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = Some(transform);
        self
    }

    /// A pen or highlighter stroke through `points`.
    pub fn stroke(kind: Kind, points: Vec<[f32; 3]>) -> Self {
        Self::new(kind, Geom::Stroke).with_points(points)
    }
}
