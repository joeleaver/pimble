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
    /// Cubic Bézier subpaths (docs/DRAWING_CONTRACT.md section 2): what the
    /// Pen and the Pencil draw, and what a live shape becomes when one of
    /// its points is edited.
    Path,
    /// A regular polygon fitted to its box (`style.sides`).
    Polygon,
    /// A star fitted to its box (`style.points`, `style.inner`).
    Star,
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
            Kind::Path => "path",
            Kind::Polygon => "polygon",
            Kind::Star => "star",
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
            "path" => Kind::Path,
            "polygon" => Kind::Polygon,
            "star" => Kind::Star,
            other => Kind::Other(other.to_string()),
        }
    }

    /// The kind as a person names one ("a sticky note", "an arrow").
    pub fn with_article(&self) -> String {
        let word = match self {
            Kind::Pen => "pen stroke",
            Kind::Highlighter => "highlighter stroke",
            Kind::Rect => "rectangle",
            Kind::Text => "text",
            Kind::Sticky => "sticky note",
            Kind::Step => "numbered step",
            Kind::Card => "node card",
            other => other.as_str(),
        };
        let article = if word.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
        format!("{article} {word}")
    }

    /// A stroke: its geometry is the appendable point array.
    pub fn is_stroke(&self) -> bool {
        matches!(self, Kind::Pen | Kind::Highlighter)
    }

    /// A live shape: a box with a look of its own (rectangle with its
    /// corner radius, ellipse, diamond, polygon, star). [`crate::path::to_path`]
    /// turns one into a path.
    pub fn is_shape(&self) -> bool {
        matches!(self, Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Polygon | Kind::Star)
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
    /// A path's subpaths and fill rule, one value like every geometry.
    Path(crate::path::PathGeom),
}

impl Geom {
    /// The geometry with every point mapped by `f` (an affine map: a
    /// handle's offset maps as `f(p + h) - f(p)`). Rectangles map by two
    /// corners. `None` for a group and a stroke (whose points are not in
    /// this value). What a geometric resize writes.
    pub fn mapped(&self, f: impl Fn(Point) -> Point) -> Option<Geom> {
        let mr = |r: &Rect| Rect::from_points(f(Point::new(r.x, r.y)), f(Point::new(r.right(), r.bottom())));
        let end = |e: &End| match e {
            End::Point(p) => End::Point(f(*p)),
            End::Element { id, side, last } => End::Element { id: id.clone(), side: *side, last: f(*last) },
        };
        Some(match self {
            Geom::None | Geom::Stroke => return None,
            Geom::Rect(r) => Geom::Rect(mr(r)),
            Geom::Picture(p) => Geom::Picture(PictureGeom { rect: mr(&p.rect), ..p.clone() }),
            Geom::Segment { a, b } => Geom::Segment { a: f(*a), b: f(*b) },
            Geom::Anchor(p) => Geom::Anchor(f(*p)),
            Geom::Callout { rect, tip } => Geom::Callout { rect: mr(rect), tip: f(*tip) },
            Geom::Connector { from, to } => Geom::Connector { from: end(from), to: end(to) },
            Geom::Card { rect, link } => Geom::Card { rect: mr(rect), link: link.clone() },
            Geom::Path(p) => Geom::Path(p.map(&f)),
        })
    }
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

/// What a layer holds (docs/DRAWING_CONTRACT.md section 1): elements (a
/// vector layer, every layer made before kinds), paint operations (a paint
/// layer), or other layers (a group). Stored as the layer's `kind`; absent
/// reads as [`LayerKind::Vector`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LayerKind {
    Vector,
    Paint,
    Group,
    /// A kind this version does not know: drawn as nothing, takes nothing.
    Other(String),
}

impl LayerKind {
    pub fn as_str(&self) -> &str {
        match self {
            LayerKind::Vector => "vector",
            LayerKind::Paint => "paint",
            LayerKind::Group => "group",
            LayerKind::Other(s) => s,
        }
    }

    pub fn parse(s: &str) -> LayerKind {
        match s {
            "" | "vector" => LayerKind::Vector,
            "paint" => LayerKind::Paint,
            "group" => LayerKind::Group,
            other => LayerKind::Other(other.to_string()),
        }
    }

    pub fn is_vector(&self) -> bool {
        matches!(self, LayerKind::Vector)
    }

    pub fn is_group(&self) -> bool {
        matches!(self, LayerKind::Group)
    }

    /// The blend mode a layer of this kind has when it names none: a group
    /// passes through, every other layer is normal.
    pub fn default_blend(&self) -> Blend {
        if self.is_group() {
            Blend::PassThrough
        } else {
            Blend::Normal
        }
    }
}

impl fmt::Display for LayerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a layer is laid over what is below it (docs/DRAWING_CONTRACT.md
/// section 1), as painting apps name the modes. `PassThrough` is a group's
/// alone: its children blend with what is below the group as if they were
/// not grouped; on any other layer it reads as `Normal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Blend {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    PassThrough,
}

impl Blend {
    /// Every mode, in the order a menu offers them.
    pub const ALL: [Blend; 13] = [
        Blend::PassThrough,
        Blend::Normal,
        Blend::Multiply,
        Blend::Screen,
        Blend::Overlay,
        Blend::Darken,
        Blend::Lighten,
        Blend::ColorDodge,
        Blend::ColorBurn,
        Blend::HardLight,
        Blend::SoftLight,
        Blend::Difference,
        Blend::Exclusion,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Blend::Normal => "normal",
            Blend::Multiply => "multiply",
            Blend::Screen => "screen",
            Blend::Overlay => "overlay",
            Blend::Darken => "darken",
            Blend::Lighten => "lighten",
            Blend::ColorDodge => "color-dodge",
            Blend::ColorBurn => "color-burn",
            Blend::HardLight => "hard-light",
            Blend::SoftLight => "soft-light",
            Blend::Difference => "difference",
            Blend::Exclusion => "exclusion",
            Blend::PassThrough => "pass-through",
        }
    }

    pub fn parse(s: &str) -> Option<Blend> {
        Blend::ALL.iter().copied().find(|b| b.as_str() == s)
    }

    /// The name a person reads ("Color Dodge").
    pub fn label(&self) -> &'static str {
        match self {
            Blend::Normal => "Normal",
            Blend::Multiply => "Multiply",
            Blend::Screen => "Screen",
            Blend::Overlay => "Overlay",
            Blend::Darken => "Darken",
            Blend::Lighten => "Lighten",
            Blend::ColorDodge => "Color Dodge",
            Blend::ColorBurn => "Color Burn",
            Blend::HardLight => "Hard Light",
            Blend::SoftLight => "Soft Light",
            Blend::Difference => "Difference",
            Blend::Exclusion => "Exclusion",
            Blend::PassThrough => "Pass Through",
        }
    }

    /// The renderer's mode (`PassThrough` composites as normal when a group
    /// needs a buffer at all, which is only below 100% opacity).
    pub fn to_tiny(&self) -> tiny_skia::BlendMode {
        use tiny_skia::BlendMode as B;
        match self {
            Blend::Normal | Blend::PassThrough => B::SourceOver,
            Blend::Multiply => B::Multiply,
            Blend::Screen => B::Screen,
            Blend::Overlay => B::Overlay,
            Blend::Darken => B::Darken,
            Blend::Lighten => B::Lighten,
            Blend::ColorDodge => B::ColorDodge,
            Blend::ColorBurn => B::ColorBurn,
            Blend::HardLight => B::HardLight,
            Blend::SoftLight => B::SoftLight,
            Blend::Difference => B::Difference,
            Blend::Exclusion => B::Exclusion,
        }
    }

    /// The CSS `mix-blend-mode` value (SVG export); `None` for normal.
    pub fn css(&self) -> Option<&'static str> {
        match self {
            Blend::Normal | Blend::PassThrough => None,
            other => Some(other.as_str()),
        }
    }
}

impl fmt::Display for Blend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

/// How the ends of an open stroke are drawn. Absent reads as round, or
/// butt for the dashed preset: the look every element had before caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cap {
    Butt,
    Round,
    Square,
}

impl Cap {
    pub const ALL: [Cap; 3] = [Cap::Butt, Cap::Round, Cap::Square];

    pub fn as_str(&self) -> &'static str {
        match self {
            Cap::Butt => "butt",
            Cap::Round => "round",
            Cap::Square => "square",
        }
    }

    pub fn parse(s: &str) -> Option<Cap> {
        Cap::ALL.iter().copied().find(|c| c.as_str() == s)
    }

    pub fn to_tiny(&self) -> tiny_skia::LineCap {
        match self {
            Cap::Butt => tiny_skia::LineCap::Butt,
            Cap::Round => tiny_skia::LineCap::Round,
            Cap::Square => tiny_skia::LineCap::Square,
        }
    }
}

/// How a stroke turns a corner. Absent reads as round (the look every
/// element had before joins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Join {
    Miter,
    Round,
    Bevel,
}

impl Join {
    pub const ALL: [Join; 3] = [Join::Miter, Join::Round, Join::Bevel];

    pub fn as_str(&self) -> &'static str {
        match self {
            Join::Miter => "miter",
            Join::Round => "round",
            Join::Bevel => "bevel",
        }
    }

    pub fn parse(s: &str) -> Option<Join> {
        Join::ALL.iter().copied().find(|j| j.as_str() == s)
    }

    pub fn to_tiny(&self) -> tiny_skia::LineJoin {
        match self {
            Join::Miter => tiny_skia::LineJoin::Miter,
            Join::Round => tiny_skia::LineJoin::Round,
            Join::Bevel => tiny_skia::LineJoin::Bevel,
        }
    }
}

/// The default miter limit (SVG's).
pub const DEFAULT_MITER: f32 = 4.0;
/// A polygon's sides when its style names none.
pub const DEFAULT_SIDES: u32 = 6;
/// A star's points when its style names none.
pub const DEFAULT_STAR_POINTS: u32 = 5;
/// A star's inner radius, as a fraction of its outer one, when its style
/// names none.
pub const DEFAULT_STAR_INNER: f32 = 0.4;

/// A custom dash pattern as stored: lengths in board units, on and off in
/// turn, space separated (`"6 3 1 3"`, SVG's `stroke-dasharray`). `None` for
/// anything that is not a list of finite non-negative numbers adding up to
/// more than zero.
pub fn parse_dash_pattern(s: &str) -> Option<Vec<f32>> {
    let v: Vec<f32> = s.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).map(|t| t.parse::<f32>().ok()).collect::<Option<_>>()?;
    (!v.is_empty() && v.iter().all(|x| x.is_finite() && *x >= 0.0) && v.iter().sum::<f32>() > 0.0).then_some(v)
}

/// A dash pattern as stored (see [`parse_dash_pattern`]).
pub fn dash_pattern_text(v: &[f32]) -> String {
    v.iter().map(|x| format!("{x}")).collect::<Vec<_>>().join(" ")
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
    /// Stroke ends (drawing wave 2): absent is round, butt when dashed.
    pub cap: Option<Cap>,
    /// Stroke corners: absent is round.
    pub join: Option<Join>,
    /// The miter limit, for a mitered join: absent is [`DEFAULT_MITER`].
    pub miter: Option<f32>,
    /// A custom dash pattern in board units (on, off, ...); when set it
    /// wins over `dash`. Stored as text ([`dash_pattern_text`]).
    pub dash_pattern: Option<Vec<f32>>,
    /// A rectangle's corner radius in board units: absent is 0.
    pub radius: Option<f32>,
    /// A polygon's sides (3 or more): absent is [`DEFAULT_SIDES`].
    pub sides: Option<u32>,
    /// A star's points (3 or more): absent is [`DEFAULT_STAR_POINTS`].
    pub points: Option<u32>,
    /// A star's inner radius as a fraction of its outer one (0 to 1):
    /// absent is [`DEFAULT_STAR_INNER`].
    pub inner: Option<f32>,
    /// A gradient fill (drawing wave 3): when set it is what fills, and
    /// `fill` is its solid fallback (what an older app draws).
    pub fill_gradient: Option<Gradient>,
    /// A gradient stroke; `stroke` is its solid fallback.
    pub stroke_gradient: Option<Gradient>,
    // Map lettering (docs/DRAWING_WAVE6_CONTRACT.md section 4): text's
    // font, halo, letter spacing, and text set along a path.
    /// The font family: absent (or a name this version does not know) is
    /// sans.
    pub font: Option<FontFamily>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    /// The halo's color: absent is white (a halo needs a width or a color).
    pub halo_color: Option<Color>,
    /// The halo's width in board units, how far it reaches past the
    /// glyphs: absent is none, or a tenth of the font size when only a
    /// color is set.
    pub halo_width: Option<f32>,
    /// Letter spacing in em: absent is 0.
    pub tracking: Option<f32>,
    /// Set along this element (a path, line, arrow or pen stroke): the
    /// text's own box is ignored while it names one ([`crate::along`]).
    pub along: Option<String>,
    /// Where along it, 0 (its start) to 1 (its end): absent is where the
    /// text's alignment puts it (left 0, center 0.5, right 1).
    pub along_offset: Option<f32>,
    /// Which side of it: absent is above.
    pub along_side: Option<AlongSide>,
}

pub use crate::text::{FontChoice, FontFamily, TextStyle};

/// Which side of its path a text set along one sits on, reading the path
/// in the text's direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AlongSide {
    /// The baseline just above the path.
    #[default]
    Above,
    /// Centered on the path (a road's name in the road).
    On,
    /// The capitals' tops just below the path.
    Below,
}

impl AlongSide {
    pub const ALL: [AlongSide; 3] = [AlongSide::Above, AlongSide::On, AlongSide::Below];

    pub fn as_str(&self) -> &'static str {
        match self {
            AlongSide::Above => "above",
            AlongSide::On => "on",
            AlongSide::Below => "below",
        }
    }

    pub fn parse(s: &str) -> Option<AlongSide> {
        Some(match s {
            "above" => AlongSide::Above,
            "on" => AlongSide::On,
            "below" => AlongSide::Below,
            _ => return None,
        })
    }
}

/// A halo: glyphs stroked behind their fill.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Halo {
    pub color: Color,
    /// How far it reaches past the glyphs, board units (the stroke is
    /// twice this wide, centered on the outline).
    pub width: f32,
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
    pub cap: Cap,
    pub join: Join,
    pub miter: f32,
    /// A custom pattern, when set (it wins over `dash`).
    pub dash_pattern: Option<Vec<f32>>,
    pub radius: f32,
    pub sides: u32,
    pub points: u32,
    pub inner: f32,
    /// The fill's gradient, when it has one (then `fill` is `Some`, its
    /// fallback).
    pub fill_gradient: Option<Gradient>,
    /// The stroke's gradient, when it has one.
    pub stroke_gradient: Option<Gradient>,
    /// The font and letter spacing its text is set in.
    pub text: TextStyle,
    pub halo: Option<Halo>,
    /// The element its text is set along, as named (whether that names a
    /// path is the snapshot's judgement).
    pub along: Option<String>,
    pub along_offset: Option<f32>,
    pub along_side: AlongSide,
}

impl ResolvedStyle {
    /// Dashed in any way (a preset or a custom pattern): a pen drawn dashed
    /// is drawn at one width.
    pub fn is_dashed(&self) -> bool {
        self.dash != Dash::Solid || self.dash_pattern.is_some()
    }

    /// How far its stroke can reach past the geometry: half the width,
    /// times the miter limit for a mitered join and √2 for square caps.
    pub fn stroke_reach(&self) -> f32 {
        let mut k: f32 = 1.0;
        if self.join == Join::Miter {
            k = k.max(self.miter);
        }
        if self.cap == Cap::Square {
            k = k.max(std::f32::consts::SQRT_2);
        }
        self.width / 2.0 * k
    }
}

// ── Gradients (docs/DRAWING_CONTRACT.md section 2, "Fill and stroke") ──

/// Linear or radial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GradientKind {
    Linear,
    Radial,
}

impl GradientKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GradientKind::Linear => "linear",
            GradientKind::Radial => "radial",
        }
    }

    pub fn parse(s: &str) -> Option<GradientKind> {
        Some(match s {
            "linear" => GradientKind::Linear,
            "radial" => GradientKind::Radial,
            _ => return None,
        })
    }
}

/// One color of a gradient at `offset` (0 to 1 along it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    pub offset: f32,
    pub color: Color,
}

impl GradientStop {
    pub fn new(offset: f32, color: Color) -> Self {
        Self { offset, color }
    }
}

/// A gradient paint for a fill or a stroke. Its handles are in the
/// element's **box units**: (0, 0) is the top-left of the element's own
/// extent before its transform ([`crate::Element::pivot_bounds`]) and (1, 1)
/// its bottom-right, as SVG's `objectBoundingBox`. So the gradient moves,
/// turns and scales with the element, and stretches with it when it is
/// resized. Handles may lie outside 0..1.
///
/// - Linear: offset 0 at `from`, offset 1 at `to`; colors pad beyond.
/// - Radial: centered on `from`, offset 1 on the circle through `to` (the
///   radius is `|to - from|` in box units, so an ellipse in a box that is
///   not square).
///
/// Stored whole (one value, last writer wins) under the style keys
/// `fill_gradient` and `stroke_gradient`; the solid `fill` / `stroke` beside
/// it is the fallback an older app draws.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub kind: GradientKind,
    pub from: Point,
    pub to: Point,
    pub stops: Vec<GradientStop>,
}

impl Gradient {
    pub fn linear(from: Point, to: Point, stops: Vec<GradientStop>) -> Self {
        Self { kind: GradientKind::Linear, from, to, stops }
    }

    pub fn radial(center: Point, edge: Point, stops: Vec<GradientStop>) -> Self {
        Self { kind: GradientKind::Radial, from: center, to: edge, stops }
    }

    /// A new linear gradient from `color` to the same color transparent,
    /// left to right across the element's box: what the color well starts
    /// a gradient with.
    pub fn default_linear(color: Color) -> Self {
        Self::linear(Point::new(0.0, 0.5), Point::new(1.0, 0.5), Self::fade(color))
    }

    /// A new radial gradient from `color` at the center to the same color
    /// transparent at the box's edge.
    pub fn default_radial(color: Color) -> Self {
        Self::radial(Point::new(0.5, 0.5), Point::new(1.0, 0.5), Self::fade(color))
    }

    fn fade(color: Color) -> Vec<GradientStop> {
        vec![GradientStop::new(0.0, color), GradientStop::new(1.0, Color { a: 0, ..color })]
    }

    /// Something to draw: finite handles, at least one stop, finite offsets
    /// (a radial one with some radius).
    pub fn is_valid(&self) -> bool {
        let finite = |p: Point| p.x.is_finite() && p.y.is_finite();
        finite(self.from)
            && finite(self.to)
            && !self.stops.is_empty()
            && self.stops.iter().all(|s| s.offset.is_finite())
            && (self.kind == GradientKind::Linear || self.radius() > 1e-6)
    }

    /// A radial gradient's radius in box units (the length of a linear one).
    pub fn radius(&self) -> f32 {
        ((self.to.x - self.from.x).powi(2) + (self.to.y - self.from.y).powi(2)).sqrt()
    }

    /// Its stops clamped to 0..1 and sorted (stable, so two stops at one
    /// offset keep their order: a hard edge).
    pub fn normalized(mut self) -> Self {
        for s in &mut self.stops {
            s.offset = s.offset.clamp(0.0, 1.0);
        }
        self.stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
        self
    }

    /// The solid color that stands for it where a gradient cannot be drawn
    /// (an older app, a picker's swatch): its first stop's color.
    pub fn fallback(&self) -> Color {
        self.stops.first().map(|s| s.color).unwrap_or(Color::BLACK)
    }

    /// The color at `t` (0 to 1 along it), as the renderer interpolates.
    pub fn color_at(&self, t: f32) -> Color {
        let stops = &self.stops;
        let Some(first) = stops.first() else { return Color::BLACK };
        if t <= first.offset {
            return first.color;
        }
        for w in stops.windows(2) {
            let (a, b) = (w[0], w[1]);
            if t <= b.offset {
                let k = if b.offset > a.offset { (t - a.offset) / (b.offset - a.offset) } else { 1.0 };
                let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round().clamp(0.0, 255.0) as u8;
                return Color::rgba(mix(a.color.r, b.color.r), mix(a.color.g, b.color.g), mix(a.color.b, b.color.b), mix(a.color.a, b.color.a));
            }
        }
        stops.last().map(|s| s.color).unwrap_or(first.color)
    }

    /// The gradient flipped within its box (a flip of the element by its
    /// geometry: [`crate::BoardTxn::flip`] mirrors through the transform,
    /// which carries the gradient along, so this is for callers that mirror
    /// geometry themselves).
    pub fn mirrored(&self, horizontal: bool) -> Self {
        let m = |p: Point| if horizontal { Point::new(1.0 - p.x, p.y) } else { Point::new(p.x, 1.0 - p.y) };
        Self { from: m(self.from), to: m(self.to), ..self.clone() }
    }

    /// The box a gradient is laid over for an element of extent `r`: `r`,
    /// with a side of no length (a horizontal line) given the other's length
    /// so the gradient still has room.
    pub fn box_for(r: Rect) -> Rect {
        let (mut b, eps) = (r.normalized(), 1e-3);
        if b.w < eps && b.h < eps {
            return Rect::new(b.x - 0.5, b.y - 0.5, 1.0, 1.0);
        }
        if b.h < eps {
            b = Rect::new(b.x, b.y - b.w / 2.0, b.w, b.w);
        } else if b.w < eps {
            b = Rect::new(b.x - b.h / 2.0, b.y, b.h, b.h);
        }
        b
    }
}

/// What a fill or a stroke is painted with, for the setters that change the
/// kind of paint ([`crate::BoardTxn::set_fill_paint`],
/// [`crate::BoardTxn::set_stroke_paint`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    /// No fill (a stroke cannot be none: give it width 0).
    None,
    Solid(Color),
    Gradient(Gradient),
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
            Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Polygon | Kind::Star | Kind::Path => (MARK_RED, None, 4.0, 24.0, ArrowHeads::None, Align::Center, false),
            Kind::Text => (MARK_RED, None, 0.0, 24.0, ArrowHeads::None, Align::Left, false),
            Kind::Sticky => (Color::rgb(0x21, 0x21, 0x21), Some(STICKY_YELLOW), 0.0, 20.0, ArrowHeads::None, Align::Left, true),
            Kind::Step => (MARK_RED, None, 0.0, 24.0, ArrowHeads::None, Align::Center, false),
            Kind::Callout => (MARK_RED, Some(Color::WHITE), 3.0, 20.0, ArrowHeads::None, Align::Left, false),
            Kind::Frame => (Color::rgb(0x9e, 0x9e, 0x9e), None, 2.0, 18.0, ArrowHeads::None, Align::Left, false),
            Kind::Card => (Color::rgb(0x61, 0x61, 0x61), Some(Color::rgb(0xfa, 0xfa, 0xfa)), 1.5, 18.0, ArrowHeads::None, Align::Left, true),
            Kind::Spotlight => (Color::BLACK, Some(Color::rgba(0, 0, 0, 0x99)), 0.0, 24.0, ArrowHeads::None, Align::Left, false),
            Kind::Picture | Kind::Group | Kind::Other(_) => (Color::BLACK, None, 0.0, 24.0, ArrowHeads::None, Align::Left, false),
        };
        let fill_gradient = self.fill_gradient.clone().filter(Gradient::is_valid).map(Gradient::normalized);
        let stroke_gradient = self.stroke_gradient.clone().filter(Gradient::is_valid).map(Gradient::normalized);
        ResolvedStyle {
            stroke: self.stroke.or(stroke_gradient.as_ref().map(Gradient::fallback)).unwrap_or(stroke),
            fill: self.fill.or(fill_gradient.as_ref().map(Gradient::fallback)).or(fill),
            width: self.width.unwrap_or(width).max(0.0),
            opacity: self.opacity.unwrap_or(1.0).clamp(0.0, 1.0),
            font_size: self.font_size.unwrap_or(font_size).max(1.0),
            align: self.align.unwrap_or(align),
            shadow: self.shadow.unwrap_or(shadow),
            arrow_heads: self.arrow_heads.unwrap_or(arrow_heads),
            dash: self.dash.unwrap_or(Dash::Solid),
            cap: self.cap.unwrap_or(if self.dash == Some(Dash::Dashed) && self.dash_pattern.is_none() { Cap::Butt } else { Cap::Round }),
            join: self.join.unwrap_or(Join::Round),
            miter: self.miter.filter(|m| m.is_finite()).unwrap_or(DEFAULT_MITER).max(1.0),
            dash_pattern: self.dash_pattern.clone().filter(|v| !v.is_empty() && v.iter().all(|x| x.is_finite() && *x >= 0.0) && v.iter().sum::<f32>() > 0.0),
            radius: self.radius.filter(|r| r.is_finite()).unwrap_or(0.0).max(0.0),
            sides: self.sides.unwrap_or(DEFAULT_SIDES).clamp(3, 1000),
            points: self.points.unwrap_or(DEFAULT_STAR_POINTS).clamp(3, 1000),
            inner: self.inner.filter(|r| r.is_finite()).unwrap_or(DEFAULT_STAR_INNER).clamp(0.0, 1.0),
            fill_gradient,
            stroke_gradient,
            text: TextStyle {
                font: FontChoice { family: self.font.unwrap_or_default(), bold: self.bold.unwrap_or(false), italic: self.italic.unwrap_or(false) },
                tracking: self.tracking.filter(|t| t.is_finite()).unwrap_or(0.0).clamp(-0.5, 2.0),
            },
            halo: {
                let width = match (self.halo_width.filter(|w| w.is_finite()), self.halo_color) {
                    (Some(w), _) => w.max(0.0),
                    (None, Some(_)) => self.font_size.unwrap_or(font_size).max(1.0) * 0.1,
                    (None, None) => 0.0,
                };
                (width > 0.0).then(|| Halo { color: self.halo_color.unwrap_or(Color::WHITE), width })
            },
            along: self.along.clone().filter(|a| !a.is_empty()),
            along_offset: self.along_offset.filter(|o| o.is_finite()).map(|o| o.clamp(0.0, 1.0)),
            along_side: self.along_side.unwrap_or_default(),
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
    Cap,
    Join,
    Miter,
    DashPattern,
    Radius,
    Sides,
    Points,
    Inner,
    FillGradient,
    StrokeGradient,
    Font,
    Bold,
    Italic,
    HaloColor,
    HaloWidth,
    Tracking,
    Along,
    AlongOffset,
    AlongSide,
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
            StyleKey::Cap => "cap",
            StyleKey::Join => "join",
            StyleKey::Miter => "miter",
            StyleKey::DashPattern => "dash_pattern",
            StyleKey::Radius => "radius",
            StyleKey::Sides => "sides",
            StyleKey::Points => "points",
            StyleKey::Inner => "inner",
            StyleKey::FillGradient => "fill_gradient",
            StyleKey::StrokeGradient => "stroke_gradient",
            StyleKey::Font => "font",
            StyleKey::Bold => "bold",
            StyleKey::Italic => "italic",
            StyleKey::HaloColor => "halo_color",
            StyleKey::HaloWidth => "halo_width",
            StyleKey::Tracking => "tracking",
            StyleKey::Along => "along",
            StyleKey::AlongOffset => "along_offset",
            StyleKey::AlongSide => "along_side",
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
    /// `None`: the top layer that is not locked.
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

    /// A path element.
    pub fn path(geom: crate::path::PathGeom) -> Self {
        Self::new(Kind::Path, Geom::Path(geom))
    }
}
