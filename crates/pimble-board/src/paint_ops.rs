//! Paint operations as stored and read (docs/DRAWING_CONTRACT.md section 3,
//! "Paint is stored as operations, not pixels"). A paint layer is the
//! ordered list of the operations naming it; every device replays the list
//! to get the image ([`crate::tiles`]).
//!
//! ```text
//! board: Map {
//!   ...
//!   "paint:<op id>": Map {
//!     layer: String, order: String,          // fractional, within the layer
//!     kind: "stroke" | "pixels" | "fill" | "smudge" | "blur" | "move",
//!     author: Any { id, name }?, created_at: String,
//!     // stroke
//!     preset: String, color: "#rrggbb(aa)", blend: "normal" | "erase",
//!     brush: Any { size, hardness, spacing, flow, opacity,
//!                  pressure_size: [at0, at1, gamma], pressure_opacity: [...],
//!                  tilt_size: [...], tilt_opacity: [...],
//!                  jitter_size, jitter_opacity, jitter_position, grain, wet_edge },
//!     points: Array<Buffer>,                  // chunks of little-endian f32
//!                                             // quadruples (x, y, pressure, tilt)
//!     // pixels
//!     src: "pimble-blob:...", rect: Any { x, y, w, h },
//!     blend: "multiply" | ...?,              // absent: normal (wave 5)
//!     // fill (wave 5)
//!     seed: Any { x, y }, color, tolerance: Number (0..1),
//!     clip: Any { x, y, w, h }, contiguous: Bool?,   // absent: true
//!     // smudge, blur (wave 5): a stroke's preset, brush and points, and
//!     strength: Number (0..1),
//!     // move (wave 5)
//!     selection: Any { kind: "rect", x, y, w, h }
//!              | Any { kind: "lasso", points: [x0, y0, x1, y1, ...] },
//!     transform: Any [a, b, c, d, e, f]?,     // absent: nothing put down
//!   },
//! }
//! ```
//!
//! **Each operation is its own key of the board map** (`paint:<op id>`),
//! where the contract drew one `paint` map of them: a map made on first use
//! would be made by two devices painting their first strokes while apart,
//! and yrs keeps only one of the two maps (and its strokes), the rule this
//! crate's `board:<id>` keys and node fields already follow. A board made
//! before painting needs nothing new; an older app ignores the keys.
//!
//! A kind this version does not know is read as [`PaintBody::Other`] and
//! skipped when drawing: the layer draws the rest.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::brush::{self, BrushPoint, BrushSettings, Preset, StrokeBlend, PX_PER_UNIT};
use crate::model::{Author, Blend, Color, Point, Rect};
use crate::snapshot::Fx;

/// The prefix of a paint operation's key in the board map.
pub const PAINT_PREFIX: &str = "paint:";

/// The board-map key of operation `id`.
pub fn paint_key(id: &str) -> String {
    format!("{PAINT_PREFIX}{id}")
}

/// A brush stroke: what it was drawn with and its points.
#[derive(Debug, Clone, PartialEq)]
pub struct StrokeOp {
    /// The preset's name as it was when drawn ("round"); a name this version
    /// does not know is kept and drawn with the stored settings.
    pub preset: String,
    pub settings: BrushSettings,
    pub color: Color,
    pub blend: StrokeBlend,
    pub points: Vec<BrushPoint>,
}

/// A picture laid over a rectangle of the layer (Flatten, Merge Down,
/// paste).
#[derive(Debug, Clone, PartialEq)]
pub struct PixelsOp {
    /// A `pimble-blob:` URL.
    pub src: String,
    /// Where it lies, board units.
    pub rect: Rect,
    /// How it is laid onto the layer's pixels (stored only when not
    /// normal: Merge Down of a layer that blends keeps its blend here).
    pub blend: Blend,
}

/// A flood fill (docs/DRAWING_CONTRACT.md section 3, "fill"): the pixels
/// of its layer, as they are at its place in the order, that match the
/// seed's within the tolerance (connected to it, unless `contiguous` is
/// off), inside the clip, painted over with the color.
#[derive(Debug, Clone, PartialEq)]
pub struct FillOp {
    /// Where it was filled, board units.
    pub seed: Point,
    pub color: Color,
    /// 0 to 1: how far a pixel's channels (premultiplied, as a fraction of
    /// 255) may be from the seed pixel's and still fill.
    pub tolerance: f32,
    /// What it may fill, board units: computed once by the filler's device
    /// ([`crate::raster::fill_clip`]), so every device fills the same area
    /// and a fill on an infinite canvas ends.
    pub clip: Rect,
    /// Only the pixels connected to the seed (4-connected); off: every
    /// matching pixel in the clip.
    pub contiguous: bool,
}

/// What a retouching brush does with the pixels under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Retouch {
    /// Drags the pixels along the stroke.
    Smudge,
    /// Softens the pixels under the stroke.
    Blur,
}

impl Retouch {
    pub fn as_str(&self) -> &'static str {
        match self {
            Retouch::Smudge => "smudge",
            Retouch::Blur => "blur",
        }
    }

    pub fn parse(s: &str) -> Option<Retouch> {
        match s {
            "smudge" => Some(Retouch::Smudge),
            "blur" => Some(Retouch::Blur),
            _ => None,
        }
    }
}

/// A smudge or blur stroke: brush settings and points like a stroke (and
/// streamed the same way), reading the pixels under it at its place in the
/// order.
#[derive(Debug, Clone, PartialEq)]
pub struct RetouchOp {
    pub effect: Retouch,
    pub preset: String,
    pub settings: BrushSettings,
    /// 0 to 1: how much each dab drags (smudge) or softens (blur).
    pub strength: f32,
    pub points: Vec<BrushPoint>,
}

/// A paint selection's region, board units.
#[derive(Debug, Clone, PartialEq)]
pub enum Selection {
    Rect(Rect),
    /// A closed polygon (the lasso's points), filled by the nonzero rule.
    Lasso(Vec<Point>),
}

impl Selection {
    /// Its bounding box; `None` when it encloses nothing.
    pub fn bounds(&self) -> Option<Rect> {
        if !self.is_valid() {
            return None;
        }
        match self {
            Selection::Rect(r) => Some(r.normalized()),
            Selection::Lasso(points) => {
                let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for p in points {
                    x0 = x0.min(p.x);
                    y0 = y0.min(p.y);
                    x1 = x1.max(p.x);
                    y1 = y1.max(p.y);
                }
                Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
            }
        }
    }

    /// Whether it encloses anything: a rectangle with a size, a lasso of
    /// three or more finite points that is not a line.
    pub fn is_valid(&self) -> bool {
        match self {
            Selection::Rect(r) => {
                let r = r.normalized();
                r.is_finite() && r.w > 0.0 && r.h > 0.0
            }
            Selection::Lasso(points) => {
                points.len() >= 3 && points.iter().all(|p| p.x.is_finite() && p.y.is_finite()) && polygon_area(points).abs() > 1e-6
            }
        }
    }

    /// Whether board point `p` is inside (a rectangle's right and bottom
    /// edges are outside; a lasso by the nonzero rule). A pixel is in the
    /// selection when its center is.
    pub fn contains(&self, p: Point) -> bool {
        match self {
            Selection::Rect(r) => {
                let r = r.normalized();
                p.x >= r.x && p.x < r.right() && p.y >= r.y && p.y < r.bottom()
            }
            Selection::Lasso(points) => winding(points, p) != 0,
        }
    }

    /// Its outline: a rectangle's four corners clockwise from the top
    /// left, or the lasso's points.
    pub fn outline(&self) -> Vec<Point> {
        match self {
            Selection::Rect(r) => {
                let r = r.normalized();
                vec![Point::new(r.x, r.y), Point::new(r.right(), r.y), Point::new(r.right(), r.bottom()), Point::new(r.x, r.bottom())]
            }
            Selection::Lasso(points) => points.clone(),
        }
    }

    /// Its outline put through `t` (where a moved selection lands).
    pub fn outline_through(&self, t: &PixelTransform) -> Vec<Point> {
        self.outline().into_iter().map(|p| t.apply(p)).collect()
    }
}

fn polygon_area(points: &[Point]) -> f32 {
    let mut a = 0.0;
    for i in 0..points.len() {
        let (p, q) = (points[i], points[(i + 1) % points.len()]);
        a += p.x * q.y - q.x * p.y;
    }
    a / 2.0
}

/// The winding number of a closed polygon around `p`.
fn winding(points: &[Point], p: Point) -> i32 {
    let mut w = 0;
    for i in 0..points.len() {
        let (a, b) = (points[i], points[(i + 1) % points.len()]);
        let side = (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y);
        if a.y <= p.y {
            if b.y > p.y && side > 0.0 {
                w += 1;
            }
        } else if b.y <= p.y && side < 0.0 {
            w -= 1;
        }
    }
    w
}

/// An affine map of the board onto itself, board units: `x' = a x + c y +
/// e`, `y' = b x + d y + f` (SVG's matrix order). What a paint selection's
/// pixels are put down through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelTransform {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Default for PixelTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl PixelTransform {
    pub const IDENTITY: PixelTransform = PixelTransform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn translate(dx: f32, dy: f32) -> Self {
        PixelTransform { e: dx, f: dy, ..Self::IDENTITY }
    }

    /// Scaled by `sx`, `sy` about `about` (negative flips).
    pub fn scale(sx: f32, sy: f32, about: Point) -> Self {
        PixelTransform { a: sx, b: 0.0, c: 0.0, d: sy, e: about.x - sx * about.x, f: about.y - sy * about.y }
    }

    /// Turned `degrees` clockwise (the y axis points down) about `about`.
    pub fn rotate(degrees: f32, about: Point) -> Self {
        let (s, c) = degrees.to_radians().sin_cos();
        PixelTransform { a: c, b: s, c: -s, d: c, e: about.x - c * about.x + s * about.y, f: about.y - s * about.x - c * about.y }
    }

    /// This, then `next`.
    pub fn then(&self, next: &PixelTransform) -> PixelTransform {
        let (m, n) = (self, next);
        PixelTransform {
            a: n.a * m.a + n.c * m.b,
            b: n.b * m.a + n.d * m.b,
            c: n.a * m.c + n.c * m.d,
            d: n.b * m.c + n.d * m.d,
            e: n.a * m.e + n.c * m.f + n.e,
            f: n.b * m.e + n.d * m.f + n.f,
        }
    }

    pub fn apply(&self, p: Point) -> Point {
        Point::new(self.a * p.x + self.c * p.y + self.e, self.b * p.x + self.d * p.y + self.f)
    }

    /// The map back; `None` when it flattens the board onto a line.
    pub fn inverse(&self) -> Option<PixelTransform> {
        let det = self.a * self.d - self.b * self.c;
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(PixelTransform { a, b, c, d, e: -(a * self.e + c * self.f), f: -(b * self.e + d * self.f) })
    }

    /// The axis-aligned box of `r` mapped.
    pub fn map_rect(&self, r: &Rect) -> Rect {
        let r = r.normalized();
        let corners = [Point::new(r.x, r.y), Point::new(r.right(), r.y), Point::new(r.right(), r.bottom()), Point::new(r.x, r.bottom())].map(|p| self.apply(p));
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in corners {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    pub fn is_finite(&self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f].iter().all(|v| v.is_finite())
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// The six numbers as stored, `[a, b, c, d, e, f]`.
    pub fn to_array(&self) -> [f32; 6] {
        [self.a, self.b, self.c, self.d, self.e, self.f]
    }

    pub fn from_array(v: [f32; 6]) -> Self {
        PixelTransform { a: v[0], b: v[1], c: v[2], d: v[3], e: v[4], f: v[5] }
    }

    /// A shift by whole paint pixels and nothing else: (dx, dy) in pixels.
    pub(crate) fn whole_pixel_shift(&self) -> Option<(i64, i64)> {
        let (dx, dy) = (self.e * PX_PER_UNIT, self.f * PX_PER_UNIT);
        (self.a == 1.0 && self.d == 1.0 && self.b == 0.0 && self.c == 0.0 && (dx - dx.round()).abs() < 1e-3 && (dy - dy.round()).abs() < 1e-3)
            .then(|| (dx.round() as i64, dy.round() as i64))
    }
}

/// A paint selection moved, scaled, turned or deleted: the pixels inside
/// `selection` are lifted and put down through `transform` (`None`:
/// nothing put down, the selection's Delete).
#[derive(Debug, Clone, PartialEq)]
pub struct MoveOp {
    pub selection: Selection,
    pub transform: Option<PixelTransform>,
}

/// What an operation does, by kind.
#[derive(Debug, Clone, PartialEq)]
pub enum PaintBody {
    Stroke(Arc<StrokeOp>),
    Pixels(PixelsOp),
    Fill(FillOp),
    /// Smudge and blur.
    Retouch(Arc<RetouchOp>),
    Move(MoveOp),
    /// A kind this version does not know: kept, skipped when drawing.
    Other(String),
}

impl PaintBody {
    pub fn kind(&self) -> &str {
        match self {
            PaintBody::Stroke(_) => "stroke",
            PaintBody::Pixels(_) => "pixels",
            PaintBody::Fill(_) => "fill",
            PaintBody::Retouch(r) => r.effect.as_str(),
            PaintBody::Move(_) => "move",
            PaintBody::Other(k) => k,
        }
    }

    /// Whether what it paints depends only on itself (a stroke, pixels): a
    /// fill, smudge, blur or move reads the pixels below it in its layer.
    pub fn is_local(&self) -> bool {
        matches!(self, PaintBody::Stroke(_) | PaintBody::Pixels(_) | PaintBody::Other(_))
    }
}

/// How far past its dabs a smudge or blur reaches (rounding to pixels).
pub(crate) const RETOUCH_MARGIN: f32 = 1.0;
/// The widest a blur dab reads around a pixel, in paint pixels.
pub(crate) const BLUR_MAX_KERNEL_PX: i64 = 8;

/// An operation as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct RawPaintOp {
    pub id: String,
    pub layer: String,
    pub order: String,
    pub body: PaintBody,
    pub author: Option<Author>,
    pub created_at: String,
}

impl RawPaintOp {
    pub fn kind(&self) -> &str {
        self.body.kind()
    }

    /// The board rectangle its paint can reach (`None` for a stroke with no
    /// points and a kind this version does not draw).
    pub fn bounds(&self) -> Option<Rect> {
        match &self.body {
            PaintBody::Stroke(s) => points_bounds(&s.points).map(|r| r.inflate(s.settings.reach())),
            PaintBody::Pixels(p) => {
                let r = p.rect.normalized();
                (r.w > 0.0 && r.h > 0.0 && r.is_finite()).then_some(r)
            }
            PaintBody::Fill(f) => {
                let r = f.clip.normalized();
                (r.w > 0.0 && r.h > 0.0 && r.is_finite() && r.contains(f.seed)).then_some(r)
            }
            PaintBody::Retouch(r) => points_bounds(&r.points).map(|b| b.inflate(r.settings.reach() + RETOUCH_MARGIN)),
            PaintBody::Move(m) => {
                let from = m.selection.bounds()?;
                match m.transform {
                    Some(t) if t.is_finite() => Some(from.union(&t.map_rect(&from))),
                    Some(_) => None,
                    None => Some(from),
                }
            }
            PaintBody::Other(_) => None,
        }
    }

    /// Where it reads the pixels below it (fill, smudge, blur, move);
    /// `None` for an operation that reads nothing.
    pub fn read_bounds(&self) -> Option<Rect> {
        match &self.body {
            PaintBody::Fill(_) => self.bounds(),
            PaintBody::Retouch(r) => {
                let b = self.bounds()?;
                Some(match r.effect {
                    Retouch::Smudge => b,
                    Retouch::Blur => b.inflate(BLUR_MAX_KERNEL_PX as f32 / PX_PER_UNIT),
                })
            }
            PaintBody::Move(m) => m.selection.bounds(),
            _ => None,
        }
    }
}

/// The bounding box of points' positions (non-finite ones skipped).
pub(crate) fn points_bounds(points: &[BrushPoint]) -> Option<Rect> {
    let mut it = points.iter().filter(|p| p[0].is_finite() && p[1].is_finite());
    let first = it.next()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first[0], first[1], first[0], first[1]);
    for p in it {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// A hash of everything about an operation that changes its pixels.
pub(crate) fn fingerprint(op: &RawPaintOp) -> u64 {
    let mut h = Fx::default();
    op.id.hash(&mut h);
    op.layer.hash(&mut h);
    op.order.hash(&mut h);
    match &op.body {
        PaintBody::Stroke(s) => {
            1u8.hash(&mut h);
            settings_hash(&s.settings, s.color, s.blend).hash(&mut h);
            s.points.len().hash(&mut h);
            for p in &s.points {
                for v in p {
                    h.write_u32(v.to_bits());
                }
            }
        }
        PaintBody::Pixels(p) => {
            2u8.hash(&mut h);
            p.src.hash(&mut h);
            for v in [p.rect.x, p.rect.y, p.rect.w, p.rect.h] {
                h.write_u32(v.to_bits());
            }
            p.blend.as_str().hash(&mut h);
        }
        PaintBody::Fill(f) => {
            4u8.hash(&mut h);
            f.color.hash(&mut h);
            f.contiguous.hash(&mut h);
            for v in [f.seed.x, f.seed.y, f.tolerance, f.clip.x, f.clip.y, f.clip.w, f.clip.h] {
                h.write_u32(v.to_bits());
            }
        }
        PaintBody::Retouch(r) => {
            5u8.hash(&mut h);
            r.effect.hash(&mut h);
            settings_hash(&r.settings, Color::TRANSPARENT, StrokeBlend::Normal).hash(&mut h);
            h.write_u32(r.strength.to_bits());
            r.points.len().hash(&mut h);
            for p in &r.points {
                for v in p {
                    h.write_u32(v.to_bits());
                }
            }
        }
        PaintBody::Move(m) => {
            6u8.hash(&mut h);
            selection_hash(&m.selection).hash(&mut h);
            match m.transform {
                Some(t) => {
                    for v in t.to_array() {
                        h.write_u32(v.to_bits());
                    }
                }
                None => 0u8.hash(&mut h),
            }
        }
        PaintBody::Other(k) => {
            3u8.hash(&mut h);
            k.hash(&mut h);
        }
    }
    h.finish()
}

/// A hash of a selection's region.
pub(crate) fn selection_hash(s: &Selection) -> u64 {
    let mut h = Fx::default();
    match s {
        Selection::Rect(r) => {
            1u8.hash(&mut h);
            for v in [r.x, r.y, r.w, r.h] {
                h.write_u32(v.to_bits());
            }
        }
        Selection::Lasso(points) => {
            2u8.hash(&mut h);
            for p in points {
                h.write_u32(p.x.to_bits());
                h.write_u32(p.y.to_bits());
            }
        }
    }
    h.finish()
}

/// What a derived result (a fill's coverage, a smudge's pixels, a move's
/// lifted pixels) depends on of the operation itself: all of it, but for a
/// move only its selection (where it is put down is laid per tile).
pub(crate) fn derived_input(op: &RawPaintOp) -> u64 {
    match &op.body {
        PaintBody::Move(m) => {
            let mut h = Fx::default();
            op.id.hash(&mut h);
            op.layer.hash(&mut h);
            selection_hash(&m.selection).hash(&mut h);
            h.finish()
        }
        _ => fingerprint(op),
    }
}

/// A hash of what a stroke is drawn with, points aside.
pub(crate) fn settings_hash(s: &BrushSettings, color: Color, blend: StrokeBlend) -> u64 {
    let mut h = Fx::default();
    color.hash(&mut h);
    blend.hash(&mut h);
    for v in settings_values(s) {
        h.write_u32(v.to_bits());
    }
    h.finish()
}

fn settings_values(s: &BrushSettings) -> [f32; 22] {
    [
        s.size,
        s.hardness,
        s.spacing,
        s.flow,
        s.opacity,
        s.pressure_size.at0,
        s.pressure_size.at1,
        s.pressure_size.gamma,
        s.pressure_opacity.at0,
        s.pressure_opacity.at1,
        s.pressure_opacity.gamma,
        s.tilt_size.at0,
        s.tilt_size.at1,
        s.tilt_size.gamma,
        s.tilt_opacity.at0,
        s.tilt_opacity.at1,
        s.tilt_opacity.gamma,
        s.jitter.size,
        s.jitter.opacity,
        s.jitter.position,
        s.grain,
        s.wet_edge,
    ]
}

/// A stroke to start ([`crate::BoardTxn::begin_stroke`]).
#[derive(Debug, Clone, PartialEq)]
pub struct NewStroke {
    /// A paint layer's id ([`crate::BoardTxn::paint_layer_for`] picks one by
    /// the contract's rule).
    pub layer: String,
    pub preset: String,
    pub settings: BrushSettings,
    pub color: Color,
    pub blend: StrokeBlend,
    /// The first points, if any (more come with
    /// [`crate::BoardTxn::append_stroke`]).
    pub points: Vec<BrushPoint>,
}

impl NewStroke {
    /// A stroke of `preset` in `color` into `layer`.
    pub fn new(layer: impl Into<String>, preset: &Preset, color: Color) -> NewStroke {
        NewStroke {
            layer: layer.into(),
            preset: preset.name.to_string(),
            settings: preset.settings,
            color,
            blend: StrokeBlend::Normal,
            points: Vec::new(),
        }
    }

    /// The eraser: the same brush, removing.
    pub fn erasing(mut self) -> NewStroke {
        self.blend = StrokeBlend::Erase;
        self
    }

    pub fn with_size(mut self, size: f32) -> NewStroke {
        self.settings = self.settings.with_size(size);
        self
    }

    pub fn with_settings(mut self, settings: BrushSettings) -> NewStroke {
        self.settings = settings.sanitized();
        self
    }

    pub fn with_points(mut self, points: Vec<BrushPoint>) -> NewStroke {
        self.points = points;
        self
    }
}

/// A fill to make ([`crate::BoardTxn::add_fill`]).
#[derive(Debug, Clone, PartialEq)]
pub struct NewFill {
    pub layer: String,
    pub seed: Point,
    pub color: Color,
    /// 0 to 1 ([`DEFAULT_TOLERANCE`] is a painting app's usual).
    pub tolerance: f32,
    /// [`crate::raster::fill_clip`] computes it.
    pub clip: Rect,
    pub contiguous: bool,
}

/// The fill tolerance a new fill starts with.
pub const DEFAULT_TOLERANCE: f32 = 0.15;

impl NewFill {
    /// A contiguous fill of `color` at `seed` within `clip`, at the default
    /// tolerance.
    pub fn new(layer: impl Into<String>, seed: Point, color: Color, clip: Rect) -> NewFill {
        NewFill { layer: layer.into(), seed, color, tolerance: DEFAULT_TOLERANCE, clip, contiguous: true }
    }

    pub fn with_tolerance(mut self, tolerance: f32) -> NewFill {
        self.tolerance = tolerance;
        self
    }

    /// Every matching pixel in the clip, connected or not.
    pub fn everywhere(mut self) -> NewFill {
        self.contiguous = false;
        self
    }
}

/// A smudge or blur stroke to start ([`crate::BoardTxn::begin_retouch`]).
#[derive(Debug, Clone, PartialEq)]
pub struct NewRetouch {
    pub layer: String,
    pub effect: Retouch,
    pub preset: String,
    pub settings: BrushSettings,
    pub strength: f32,
    pub points: Vec<BrushPoint>,
}

/// The strength a new smudge or blur starts with.
pub const DEFAULT_STRENGTH: f32 = 0.5;

impl NewRetouch {
    /// A smudge or blur with `preset`'s brush into `layer`.
    pub fn new(layer: impl Into<String>, effect: Retouch, preset: &Preset) -> NewRetouch {
        NewRetouch { layer: layer.into(), effect, preset: preset.name.to_string(), settings: preset.settings, strength: DEFAULT_STRENGTH, points: Vec::new() }
    }

    pub fn with_size(mut self, size: f32) -> NewRetouch {
        self.settings = self.settings.with_size(size);
        self
    }

    pub fn with_settings(mut self, settings: BrushSettings) -> NewRetouch {
        self.settings = settings.sanitized();
        self
    }

    pub fn with_strength(mut self, strength: f32) -> NewRetouch {
        self.strength = strength;
        self
    }

    pub fn with_points(mut self, points: Vec<BrushPoint>) -> NewRetouch {
        self.points = points;
        self
    }
}

/// The preset a stroke names, when this version has it.
pub fn preset_of(stroke: &StrokeOp) -> Option<&'static Preset> {
    brush::preset(&stroke.preset)
}

pub(crate) fn settings_fields(s: &BrushSettings) -> Vec<(&'static str, SettingValue)> {
    vec![
        ("size", SettingValue::N(s.size)),
        ("hardness", SettingValue::N(s.hardness)),
        ("spacing", SettingValue::N(s.spacing)),
        ("flow", SettingValue::N(s.flow)),
        ("opacity", SettingValue::N(s.opacity)),
        ("pressure_size", SettingValue::Curve(s.pressure_size)),
        ("pressure_opacity", SettingValue::Curve(s.pressure_opacity)),
        ("tilt_size", SettingValue::Curve(s.tilt_size)),
        ("tilt_opacity", SettingValue::Curve(s.tilt_opacity)),
        ("jitter_size", SettingValue::N(s.jitter.size)),
        ("jitter_opacity", SettingValue::N(s.jitter.opacity)),
        ("jitter_position", SettingValue::N(s.jitter.position)),
        ("grain", SettingValue::N(s.grain)),
        ("wet_edge", SettingValue::N(s.wet_edge)),
    ]
}

/// One stored brush setting: a number or a curve `[at0, at1, gamma]`.
pub(crate) enum SettingValue {
    N(f32),
    Curve(brush::Curve),
}

/// The settings read from stored fields over `base` (the named preset's,
/// so a field a later version adds reads as the preset's value here).
pub(crate) fn settings_from(base: BrushSettings, get: impl Fn(&str) -> Option<Vec<f32>>) -> BrushSettings {
    let mut s = base;
    let n = |k: &str, v: &mut f32| {
        if let Some(x) = get(k).and_then(|x| x.first().copied()) {
            *v = x;
        }
    };
    let c = |k: &str, v: &mut brush::Curve| {
        if let Some(x) = get(k) {
            if x.len() >= 3 {
                *v = brush::Curve::new(x[0], x[1], x[2]);
            }
        }
    };
    n("size", &mut s.size);
    n("hardness", &mut s.hardness);
    n("spacing", &mut s.spacing);
    n("flow", &mut s.flow);
    n("opacity", &mut s.opacity);
    c("pressure_size", &mut s.pressure_size);
    c("pressure_opacity", &mut s.pressure_opacity);
    c("tilt_size", &mut s.tilt_size);
    c("tilt_opacity", &mut s.tilt_opacity);
    n("jitter_size", &mut s.jitter.size);
    n("jitter_opacity", &mut s.jitter.opacity);
    n("jitter_position", &mut s.jitter.position);
    n("grain", &mut s.grain);
    n("wet_edge", &mut s.wet_edge);
    s.sanitized()
}

pub(crate) fn pack_brush_points(points: &[BrushPoint]) -> Vec<u8> {
    let mut out = Vec::with_capacity(points.len() * 16);
    for p in points {
        for v in p {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

pub(crate) fn unpack_brush_points(bytes: &[u8], out: &mut Vec<BrushPoint>) {
    for c in bytes.chunks_exact(16) {
        let f = |i: usize| f32::from_le_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
        out.push([f(0), f(4), f(8), f(12)]);
    }
}

/// A paint operation resolved for drawing: what it is, where it reaches,
/// and its fingerprint.
#[derive(Debug, Clone)]
pub struct PaintOp {
    pub raw: RawPaintOp,
    pub bounds: Option<Rect>,
    pub fingerprint: u64,
    /// The fingerprint and, for an operation that reads the pixels below
    /// it (fill, smudge, blur, move), the effective fingerprints of the
    /// operations below it in its layer that reach where it reads: what a
    /// tile keyed by it depends on (set when the snapshot indexes its layer).
    pub effective: u64,
    /// For an operation that reads the pixels below it: a hash of the
    /// effective fingerprints of those operations alone.
    pub(crate) below: u64,
    /// The key of its derived result (fill coverage, retouched pixels, a
    /// move's lifted pixels); 0 for an operation that has none.
    pub(crate) derived: u64,
}

impl PaintOp {
    pub fn of(raw: RawPaintOp) -> PaintOp {
        let bounds = raw.bounds();
        let fingerprint = fingerprint(&raw);
        PaintOp { raw, bounds, fingerprint, effective: fingerprint, below: 0, derived: 0 }
    }

    /// Whether it draws in a tile of `rect`.
    pub(crate) fn touches(&self, rect: &Rect) -> bool {
        self.bounds.is_some_and(|b| b.intersects(rect)) && !matches!(self.raw.body, PaintBody::Other(_))
    }

    pub fn id(&self) -> &str {
        &self.raw.id
    }
}

/// Set the effective fingerprints of one layer's operations (bottom to
/// top): an operation that reads the pixels below it depends on every
/// operation below it whose paint reaches where it reads, so a change there
/// changes it, and every tile and raster cache keyed by it.
pub(crate) fn settle_layer(ops: &mut [PaintOp]) {
    for i in 0..ops.len() {
        if ops[i].raw.body.is_local() {
            ops[i].effective = ops[i].fingerprint;
            ops[i].below = 0;
            ops[i].derived = 0;
            continue;
        }
        let mut h = Fx::default();
        if let Some(read) = ops[i].raw.read_bounds() {
            for below in &ops[..i] {
                if below.touches(&read) {
                    crate::snapshot::id_hash(&below.raw.id).hash(&mut h);
                    below.effective.hash(&mut h);
                }
            }
        }
        let below = h.finish();
        let mut d = Fx::default();
        derived_input(&ops[i].raw).hash(&mut d);
        below.hash(&mut d);
        let derived = d.finish() | 1;
        let mut e = Fx::default();
        ops[i].fingerprint.hash(&mut e);
        derived.hash(&mut e);
        ops[i].below = below;
        ops[i].derived = derived;
        ops[i].effective = e.finish();
    }
}
