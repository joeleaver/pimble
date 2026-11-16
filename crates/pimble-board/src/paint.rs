//! What an element paints, as a list of fills and strokes of tiny-skia
//! paths: the one description of an element's look that the raster renderer
//! ([`crate::render`]) draws and the SVG writer ([`crate::svg`]) serializes,
//! so the two agree by construction. Pictures are the one part each draws
//! its own way.

use tiny_skia::{FillRule, Path, PathBuilder, Stroke, StrokeDash};

use crate::model::*;
use crate::snapshot::{head_size, Element, STEP_RADIUS};

/// A path an op owns, or one the element keeps (its glyph outlines).
pub(crate) enum PathRef<'a> {
    Owned(Path),
    Borrowed(&'a Path),
}

impl PathRef<'_> {
    pub(crate) fn get(&self) -> &Path {
        match self {
            PathRef::Owned(p) => p,
            PathRef::Borrowed(p) => p,
        }
    }
}

/// Which space an op's path is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Space {
    /// Element space: drawn through the element's `world`.
    Element,
    /// Board units directly (connectors, a spotlight's dimming).
    Board,
}

/// How a stroke is drawn.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StrokeStyle {
    pub width: f32,
    pub dash: Dash,
    /// `None`: round, butt for the dashed preset (the look before caps).
    pub cap: Option<Cap>,
    pub join: Join,
    pub miter: f32,
    /// A custom pattern in board units; wins over `dash`.
    pub pattern: Option<Vec<f32>>,
}

impl StrokeStyle {
    /// Round caps and joins, butt caps when dashed: the look of a stroke
    /// that names no stroke detail.
    pub(crate) fn plain(width: f32, dash: Dash) -> Self {
        Self { width, dash, cap: None, join: Join::Round, miter: DEFAULT_MITER, pattern: None }
    }

    /// Everything the element's style says about its stroke.
    pub(crate) fn of(s: &ResolvedStyle) -> Self {
        Self { width: s.width, dash: s.dash, cap: Some(s.cap), join: s.join, miter: s.miter, pattern: s.dash_pattern.clone() }
    }

    pub(crate) fn with_width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub(crate) fn cap(&self) -> Cap {
        self.cap.unwrap_or(if self.dash == Dash::Dashed && self.pattern.is_none() { Cap::Butt } else { Cap::Round })
    }

    /// The dash intervals (on, off, ...), `None` when solid. An odd custom
    /// list repeats once, as SVG reads it.
    pub(crate) fn intervals(&self) -> Option<Vec<f32>> {
        if let Some(p) = &self.pattern {
            let mut v: Vec<f32> = p.iter().map(|x| x.max(0.0)).collect();
            if v.len() % 2 == 1 {
                v.extend_from_within(..);
            }
            // tiny-skia takes no zero-length dash: a dot is a hair long.
            for x in v.iter_mut().step_by(2) {
                *x = x.max(0.01);
            }
            return (v.iter().sum::<f32>() > 0.0).then_some(v);
        }
        match self.dash {
            Dash::Solid => None,
            Dash::Dashed => Some(vec![self.width * 3.0, self.width * 2.0]),
            Dash::Dotted => Some(vec![0.01, self.width * 2.0]),
        }
    }

    /// As tiny-skia's stroke.
    pub(crate) fn tiny(&self) -> Stroke {
        let width = self.width.max(0.01);
        let mut s = Stroke {
            width,
            line_cap: self.cap().to_tiny(),
            line_join: self.join.to_tiny(),
            miter_limit: self.miter.max(1.0),
            ..Stroke::default()
        };
        if let Some(v) = (StrokeStyle { width, ..self.clone() }).intervals() {
            s.dash = StrokeDash::new(v, 0.0);
        }
        s
    }
}

/// One thing an element paints.
pub(crate) enum Op<'a> {
    Fill {
        path: PathRef<'a>,
        color: Color,
        /// Multiplies the color's alpha.
        opacity: f32,
        rule: FillRule,
        space: Space,
        /// Offset by the drop shadow's offset (in its space).
        shadow: bool,
        /// Multiply blending (the highlighter).
        multiply: bool,
    },
    Stroke {
        path: PathRef<'a>,
        color: Color,
        opacity: f32,
        stroke: StrokeStyle,
        space: Space,
        multiply: bool,
    },
    /// A picture, in element space (each output draws it its own way).
    Picture(&'a PictureGeom),
}

impl<'a> Op<'a> {
    fn fill(path: PathRef<'a>, color: Color, opacity: f32, space: Space) -> Self {
        Op::Fill { path, color, opacity, rule: FillRule::Winding, space, shadow: false, multiply: false }
    }

    fn shadow(path: Path, opacity: f32) -> Self {
        Self::shadow_with(path, opacity, FillRule::Winding)
    }

    fn shadow_with(path: Path, opacity: f32, rule: FillRule) -> Self {
        Op::Fill {
            path: PathRef::Owned(path),
            color: SHADOW_COLOR,
            opacity,
            rule,
            space: Space::Element,
            shadow: true,
            multiply: false,
        }
    }

    fn stroke(path: PathRef<'a>, color: Color, opacity: f32, stroke: StrokeStyle, space: Space) -> Self {
        Op::Stroke { path, color, opacity, stroke, space, multiply: false }
    }
}

/// A drop shadow's color (before the element's opacity).
pub(crate) const SHADOW_COLOR: Color = Color::rgba(0, 0, 0, 56);
/// Text drawn this small (pixels) is drawn as bars.
pub(crate) const GREEK_BELOW_PX: f32 = 4.0;

pub(crate) fn rect_path(r: &Rect) -> Option<Path> {
    tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01)).map(PathBuilder::from_rect)
}

/// A diamond's corners: top, right, bottom, left.
pub(crate) fn diamond_points(r: &Rect) -> [Point; 4] {
    let c = r.center();
    [Point::new(c.x, r.y), Point::new(r.right(), c.y), Point::new(c.x, r.bottom()), Point::new(r.x, c.y)]
}

pub(crate) fn polygon_path(pts: &[Point]) -> Option<Path> {
    let (first, rest) = pts.split_first()?;
    let mut pb = PathBuilder::new();
    pb.move_to(first.x, first.y);
    for p in rest {
        pb.line_to(p.x, p.y);
    }
    pb.close();
    pb.finish()
}

/// A live shape's outline: rectangle (rounded by its style's radius),
/// ellipse, diamond, polygon, star.
pub(crate) fn shape_path(kind: &Kind, r: &Rect, s: &ResolvedStyle) -> Option<Path> {
    match kind {
        Kind::Ellipse => tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01)).and_then(PathBuilder::from_oval),
        Kind::Diamond => polygon_path(&diamond_points(r)),
        Kind::Rect if s.radius <= 0.0 => rect_path(r),
        Kind::Rect | Kind::Polygon | Kind::Star => crate::path::shape(kind, r, s)?.to_tiny(),
        _ => rect_path(r),
    }
}

/// An arrow head at `tip` pointing away from `from`: its triangle (tip,
/// then the two base corners) and where the line should stop so its round
/// cap does not show past the head.
pub(crate) fn arrow_head_points(tip: Point, from: Point, width: f32) -> Option<([Point; 3], Point)> {
    let (dx, dy) = (tip.x - from.x, tip.y - from.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-3 {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    let size = head_size(width).min(len * 0.9);
    let half = size * 0.5;
    let base = Point::new(tip.x - ux * size, tip.y - uy * size);
    let tri = [tip, Point::new(base.x - uy * half, base.y + ux * half), Point::new(base.x + uy * half, base.y - ux * half)];
    let stop = Point::new(tip.x - ux * size * 0.7, tip.y - uy * size * 0.7);
    Some((tri, stop))
}

/// A line or arrow's heads and its shortened line, per its style.
pub(crate) fn segment_parts(s: &ResolvedStyle, a: Point, b: Point) -> (Vec<[Point; 3]>, Point, Point) {
    let mut heads = Vec::new();
    let (mut a2, mut b2) = (a, b);
    if s.arrow_heads.end() {
        if let Some((tri, stop)) = arrow_head_points(b, a, s.width) {
            heads.push(tri);
            b2 = stop;
        }
    }
    if s.arrow_heads.start() {
        if let Some((tri, stop)) = arrow_head_points(a, b, s.width) {
            heads.push(tri);
            a2 = stop;
        }
    }
    (heads, a2, b2)
}

/// A callout's outline: its box plus the pointer from the box's center to
/// the tip, as one path (in element space).
pub(crate) fn callout_pointer(r: &Rect, tip: Point) -> [Point; 3] {
    let c = r.center();
    let (dx, dy) = (tip.x - c.x, tip.y - c.y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    let half = (r.w.min(r.h) * 0.2).max(4.0);
    [Point::new(c.x - dy / len * half, c.y + dx / len * half), tip, Point::new(c.x + dy / len * half, c.y - dx / len * half)]
}

fn callout_path(r: &Rect, tip: Point) -> Option<Path> {
    let mut pb = PathBuilder::new();
    pb.push_rect(tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01))?);
    let [mut p0, p1, mut p2] = callout_pointer(r, tip);
    // Wound like the box (clockwise on screen), so where the pointer overlaps
    // the box the two fill as one under the nonzero rule; wound the other way
    // the overlap cancelled out and the pointer's outline showed through the
    // box and its text.
    if (p1.x - p0.x) * (p2.y - p0.y) - (p1.y - p0.y) * (p2.x - p0.x) < 0.0 {
        std::mem::swap(&mut p0, &mut p2);
    }
    pb.move_to(p0.x, p0.y);
    pb.line_to(p1.x, p1.y);
    pb.line_to(p2.x, p2.y);
    pb.close();
    pb.finish()
}

/// A card's small square where the node's icon goes (element space).
pub(crate) fn card_icon_rect(s: &ResolvedStyle, r: &Rect) -> Rect {
    let pad = s.font_size * 0.6;
    let icon = s.font_size * 1.2;
    Rect::new(r.x + pad, r.y + (r.h - icon) / 2.0, icon, icon)
}

/// A stroke's points as one path (a straight polyline: smoothing happens
/// once, when the drawer releases, contract section 3).
pub(crate) fn stroke_path(points: &[[f32; 3]]) -> Option<Path> {
    let mut it = points.iter().filter(|p| p[0].is_finite() && p[1].is_finite());
    let first = it.next()?;
    let mut pb = PathBuilder::new();
    pb.move_to(first[0], first[1]);
    let mut any = false;
    for p in it {
        pb.line_to(p[0], p[1]);
        any = true;
    }
    if !any {
        // A dot: a zero-length segment, which a round cap draws as a disc.
        pb.line_to(first[0] + 0.01, first[1]);
    }
    pb.finish()
}

/// A pen stroke's points closed, for its fill; `None` under three points.
pub(crate) fn filled_stroke_path(points: &[[f32; 3]]) -> Option<Path> {
    let pts: Vec<&[f32; 3]> = points.iter().filter(|p| p[0].is_finite() && p[1].is_finite()).collect();
    if pts.len() < 3 {
        return None;
    }
    let mut pb = PathBuilder::new();
    pb.move_to(pts[0][0], pts[0][1]);
    for p in &pts[1..] {
        pb.line_to(p[0], p[1]);
    }
    pb.close();
    pb.finish()
}

/// How much of its style's width a pen stroke is drawn at, at a point's
/// pressure: exactly its width at 0.5 (a mouse, a touch screen that cannot
/// tell, and every stroke drawn before pressure was), a quarter of it with no
/// pressure, 1.75 times it at full pressure.
pub fn pressure_width(pressure: f32) -> f32 {
    let p = if pressure.is_finite() { pressure.clamp(0.0, 1.0) } else { 0.5 };
    0.25 + 1.5 * p
}

/// How a pen's or highlighter's points are drawn.
pub(crate) enum StrokeShape {
    /// One width all along: the polyline, stroked with round caps and joins
    /// (a highlighter, a dashed pen, a pen at one pressure).
    Line(Path, StrokeStyle),
    /// A pen whose pressure varies: its outline, filled (nonzero), as the
    /// union of a disc at every point and the band joining each pair of
    /// neighbors' discs.
    Outline(Path),
}

/// The width a stroke of `kind` and `width` is drawn at a point of
/// `pressure`: pressure widens a solid pen only.
pub(crate) fn width_at(kind: &Kind, width: f32, dash: Dash, pressure: f32) -> f32 {
    if matches!(kind, Kind::Pen) && dash == Dash::Solid {
        (width * pressure_width(pressure)).max(0.5)
    } else {
        width.max(0.5)
    }
}

/// The widest a stroke's points are drawn, for its bounds and hit testing.
pub(crate) fn stroke_max_width(kind: &Kind, points: &[[f32; 3]], width: f32, dash: Dash) -> f32 {
    points
        .iter()
        .filter(|p| p[0].is_finite() && p[1].is_finite())
        .map(|p| width_at(kind, width, dash, p[2]))
        .fold(0.0, f32::max)
        .max(width_at(kind, width, dash, 0.0))
}

/// How `points` of a `kind` stroke styled `width` and `dash` are drawn.
pub(crate) fn stroke_shape(kind: &Kind, points: &[[f32; 3]], width: f32, dash: Dash) -> Option<StrokeShape> {
    let finite: Vec<[f32; 3]> = points.iter().copied().filter(|p| p[0].is_finite() && p[1].is_finite()).collect();
    let first = width_at(kind, width, dash, finite.first()?[2]);
    if finite.iter().all(|p| (width_at(kind, width, dash, p[2]) - first).abs() < 1e-4) {
        return Some(StrokeShape::Line(stroke_path(&finite)?, StrokeStyle::plain(first, dash)));
    }
    let discs: Vec<(f32, f32, f32)> = finite.iter().map(|p| (p[0], p[1], width_at(kind, width, dash, p[2]) / 2.0)).collect();
    pressure_outline(&discs).map(StrokeShape::Outline)
}

/// Twice the signed area of a closed polygon.
fn signed_area(pts: &[(f32, f32)]) -> f32 {
    let mut a = 0.0;
    for i in 0..pts.len() {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[(i + 1) % pts.len()];
        a += x0 * y1 - x1 * y0;
    }
    a
}

/// The sign of the area tiny-skia's circles wind with, so every band can
/// wind the same way and nonzero filling makes their union (no holes where
/// two parts overlap).
fn circle_winding() -> f32 {
    let Some(circle) = PathBuilder::from_circle(0.0, 0.0, 1.0) else { return 1.0 };
    let mut pts = Vec::new();
    for seg in circle.segments() {
        match seg {
            tiny_skia::PathSegment::MoveTo(p) | tiny_skia::PathSegment::LineTo(p) => pts.push((p.x, p.y)),
            tiny_skia::PathSegment::QuadTo(_, p) => pts.push((p.x, p.y)),
            tiny_skia::PathSegment::CubicTo(_, _, p) => pts.push((p.x, p.y)),
            tiny_skia::PathSegment::Close => {}
        }
    }
    signed_area(&pts).signum()
}

/// A variable-width stroke's outline from its discs `(x, y, radius)`.
fn pressure_outline(discs: &[(f32, f32, f32)]) -> Option<Path> {
    let winding = circle_winding();
    let mut pb = PathBuilder::new();
    for &(x, y, r) in discs {
        pb.push_circle(x, y, r.max(0.01));
    }
    for pair in discs.windows(2) {
        let [(x0, y0, r0), (x1, y1, r1)] = [pair[0], pair[1]];
        let (dx, dy) = (x1 - x0, y1 - y0);
        let d = (dx * dx + dy * dy).sqrt();
        // One disc inside the other: the discs are the whole of it.
        if d <= (r1 - r0).abs() + 1e-4 {
            continue;
        }
        let (ux, uy) = (dx / d, dy / d);
        // The outer tangents touch each disc where its normal n has
        // n · u = (r0 - r1) / d.
        let k = (r0 - r1) / d;
        let s = (1.0 - k * k).max(0.0).sqrt();
        let (nax, nay) = (k * ux - s * uy, k * uy + s * ux);
        let (nbx, nby) = (k * ux + s * uy, k * uy - s * ux);
        let mut quad = [
            (x0 + r0 * nax, y0 + r0 * nay),
            (x1 + r1 * nax, y1 + r1 * nay),
            (x1 + r1 * nbx, y1 + r1 * nby),
            (x0 + r0 * nbx, y0 + r0 * nby),
        ];
        if signed_area(&quad).signum() != winding {
            quad.reverse();
        }
        pb.move_to(quad[0].0, quad[0].1);
        for p in &quad[1..] {
            pb.line_to(p.0, p.1);
        }
        pb.close();
    }
    pb.finish()
}

fn segment_ops<'a>(out: &mut Vec<Op<'a>>, e: &Element, a: Point, b: Point, space: Space) {
    let s = &e.style;
    let (heads, a2, b2) = segment_parts(s, a, b);
    for tri in heads {
        if let Some(path) = polygon_path(&tri) {
            out.push(Op::fill(PathRef::Owned(path), s.stroke, e.opacity, space));
        }
    }
    let mut pb = PathBuilder::new();
    pb.move_to(a2.x, a2.y);
    pb.line_to(b2.x, b2.y);
    if let Some(path) = pb.finish() {
        out.push(Op::stroke(PathRef::Owned(path), s.stroke, e.opacity, StrokeStyle::of(s), space));
    }
}

/// Its text in `color`: glyph outlines, or bars when `px_per_unit` (the
/// output's pixels per board unit, through the element's transform) makes
/// it smaller than [`GREEK_BELOW_PX`]. `None` never draws bars (vector
/// output).
fn text_ops<'a>(out: &mut Vec<Op<'a>>, e: &'a Element, color: Color, space: Space, px_per_unit: Option<f32>) {
    let (Some(layout), Some(tb)) = (&e.layout, e.text_box) else { return };
    if layout.is_empty() {
        return;
    }
    if px_per_unit.is_some_and(|k| layout.font_size * k < GREEK_BELOW_PX) {
        for r in layout.line_boxes(Point::new(tb.x, tb.y), tb.w, e.style.align) {
            if let Some(path) = rect_path(&r) {
                out.push(Op::fill(PathRef::Owned(path), color, e.opacity * 0.35, space));
            }
        }
        return;
    }
    if let Some(path) = e.text_path() {
        out.push(Op::fill(PathRef::Borrowed(path), color, e.opacity, space));
    }
}

/// Shadow, fill and outline of `path` per the element's style.
fn fill_and_stroke_ops<'a>(out: &mut Vec<Op<'a>>, e: &Element, path: Path) {
    fill_and_stroke_ops_with(out, e, path, FillRule::Winding);
}

/// [`fill_and_stroke_ops`] filling by `rule` (a path's holes).
fn fill_and_stroke_ops_with<'a>(out: &mut Vec<Op<'a>>, e: &Element, path: Path, rule: FillRule) {
    let s = &e.style;
    if s.shadow {
        out.push(Op::shadow_with(path.clone(), e.opacity, rule));
    }
    if let Some(fill) = s.fill {
        out.push(Op::Fill {
            path: PathRef::Owned(path.clone()),
            color: fill,
            opacity: e.opacity,
            rule,
            space: Space::Element,
            shadow: false,
            multiply: false,
        });
    }
    if s.width > 0.0 {
        out.push(Op::stroke(PathRef::Owned(path), s.stroke, e.opacity, StrokeStyle::of(s), Space::Element));
    }
}

/// Everything `e` paints, bottom to top. `region` is the board rectangle
/// being drawn (a spotlight dims all of it); `px_per_unit` is the output's
/// scale through the element's transform, `None` for vector output.
pub(crate) fn element_ops<'a>(e: &'a Element, region: &Rect, px_per_unit: Option<f32>) -> Vec<Op<'a>> {
    let mut out = Vec::new();
    if !e.visible || e.opacity <= 0.0 {
        return out;
    }
    let s = &e.style;
    let el = Space::Element;
    match (&e.raw.kind, &e.raw.geom) {
        (Kind::Pen | Kind::Highlighter, _) => {
            let multiply = matches!(e.raw.kind, Kind::Highlighter);
            // A pen with a fill is a freehand shape: its points closed and
            // filled under the line (contour bands, a room, a blob).
            if let (Kind::Pen, Some(fill)) = (&e.raw.kind, s.fill.filter(|f| f.a > 0)) {
                if let Some(path) = filled_stroke_path(&e.raw.points) {
                    out.push(Op::Fill {
                        path: PathRef::Owned(path),
                        color: fill,
                        opacity: e.opacity,
                        rule: FillRule::Winding,
                        space: el,
                        shadow: false,
                        multiply: false,
                    });
                }
            }
            match stroke_shape(&e.raw.kind, &e.raw.points, s.width, s.dash) {
                Some(StrokeShape::Line(path, stroke)) => out.push(Op::Stroke {
                    path: PathRef::Owned(path),
                    color: s.stroke,
                    opacity: e.opacity,
                    stroke,
                    space: el,
                    multiply,
                }),
                Some(StrokeShape::Outline(path)) => out.push(Op::Fill {
                    path: PathRef::Owned(path),
                    color: s.stroke,
                    opacity: e.opacity,
                    rule: FillRule::Winding,
                    space: el,
                    shadow: false,
                    multiply,
                }),
                None => {}
            }
        }
        (Kind::Picture, Geom::Picture(p)) => {
            if e.style.shadow {
                if let Some(path) = rect_path(&p.rect.normalized()) {
                    out.push(Op::shadow(path, e.opacity));
                }
            }
            out.push(Op::Picture(p));
        }
        (Kind::Line | Kind::Arrow, Geom::Segment { a, b }) => segment_ops(&mut out, e, *a, *b, el),
        (Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Polygon | Kind::Star, Geom::Rect(r)) => {
            if let Some(path) = shape_path(&e.raw.kind, &r.normalized(), s) {
                fill_and_stroke_ops(&mut out, e, path);
            }
            text_ops(&mut out, e, s.stroke, el, px_per_unit);
        }
        (Kind::Path, Geom::Path(p)) => {
            if let Some(path) = p.to_tiny() {
                fill_and_stroke_ops_with(&mut out, e, path, p.rule.to_tiny());
            }
        }
        (Kind::Text, Geom::Rect(_)) => {
            if let Some(fill) = s.fill {
                if let Some(path) = e.text_box.and_then(|tb| rect_path(&tb.inflate(s.font_size * 0.2))) {
                    out.push(Op::fill(PathRef::Owned(path), fill, e.opacity, el));
                }
            }
            text_ops(&mut out, e, s.stroke, el, px_per_unit);
        }
        (Kind::Sticky, Geom::Rect(r)) => {
            let g = e.grown.unwrap_or(*r).normalized();
            if let Some(path) = rect_path(&g) {
                if s.shadow {
                    out.push(Op::shadow(path.clone(), e.opacity));
                }
                let fill = s.fill.unwrap_or(STICKY_YELLOW);
                out.push(Op::fill(PathRef::Owned(path.clone()), fill, e.opacity, el));
                if s.width > 0.0 {
                    out.push(Op::stroke(PathRef::Owned(path), s.stroke, e.opacity, StrokeStyle::of(s), el));
                }
            }
            text_ops(&mut out, e, s.stroke, el, px_per_unit);
        }
        (Kind::Step, Geom::Anchor(c)) => {
            let rad = s.font_size * STEP_RADIUS;
            if let Some(path) = PathBuilder::from_circle(c.x, c.y, rad) {
                if s.shadow {
                    out.push(Op::shadow(path.clone(), e.opacity));
                }
                out.push(Op::fill(PathRef::Owned(path), s.stroke, e.opacity, el));
            }
            text_ops(&mut out, e, s.fill.unwrap_or(Color::WHITE), el, px_per_unit);
        }
        (Kind::Callout, Geom::Callout { rect, tip }) => {
            if let Some(path) = callout_path(&rect.normalized(), *tip) {
                if s.shadow {
                    out.push(Op::shadow(path.clone(), e.opacity));
                }
                // Stroke first, fill over it: the outline shows only outside
                // the union of the box and the pointer.
                if s.width > 0.0 {
                    out.push(Op::stroke(
                        PathRef::Owned(path.clone()),
                        s.stroke,
                        e.opacity,
                        StrokeStyle::plain(s.width * 2.0, Dash::Solid),
                        el,
                    ));
                }
                if let Some(fill) = s.fill {
                    out.push(Op::fill(PathRef::Owned(path), fill, e.opacity.min(1.0), el));
                }
            }
            text_ops(&mut out, e, Color::rgb(0x21, 0x21, 0x21), el, px_per_unit);
        }
        (Kind::Connector, _) => {
            if let Some((a, b)) = e.route {
                let bd = Space::Board;
                segment_ops(&mut out, e, a, b, bd);
                if let (Some(layout), Some(tb)) = (&e.layout, e.text_box) {
                    if !layout.is_empty() {
                        if let Some(bg) = rect_path(&tb.inflate(s.font_size * 0.25)) {
                            out.push(Op::fill(PathRef::Owned(bg), Color::WHITE, e.opacity, bd));
                        }
                        // A connector's world is the identity: the output's
                        // scale is the element's.
                        text_ops(&mut out, e, s.stroke, bd, px_per_unit);
                    }
                }
            }
        }
        (Kind::Frame, Geom::Rect(r)) => {
            if let Some(path) = rect_path(&r.normalized()) {
                if let Some(fill) = s.fill {
                    out.push(Op::fill(PathRef::Owned(path.clone()), fill, e.opacity, el));
                }
                out.push(Op::stroke(PathRef::Owned(path), s.stroke, e.opacity, StrokeStyle::of(s).with_width(s.width.max(0.5)), el));
            }
            text_ops(&mut out, e, Color::rgb(0x61, 0x61, 0x61), el, px_per_unit);
        }
        (Kind::Card, Geom::Card { rect, .. }) => {
            let r = rect.normalized();
            if let Some(path) = rect_path(&r) {
                fill_and_stroke_ops(&mut out, e, path);
            }
            if let Some(path) = rect_path(&card_icon_rect(s, &r)) {
                match e.card.as_ref().and_then(|c| c.color) {
                    Some(c) => out.push(Op::fill(PathRef::Owned(path), c, e.opacity, el)),
                    None => out.push(Op::fill(PathRef::Owned(path), s.stroke, e.opacity * 0.5, el)),
                }
            }
            text_ops(&mut out, e, Color::rgb(0x21, 0x21, 0x21), el, px_per_unit);
        }
        (Kind::Spotlight, Geom::Rect(r)) => {
            // Everything outside the rectangle is dimmed, across the region.
            let mut pb = PathBuilder::new();
            if let Some(outer) = tiny_skia::Rect::from_xywh(region.x - 1.0, region.y - 1.0, region.w + 2.0, region.h + 2.0) {
                pb.push_rect(outer);
            }
            if let Some(inner) = rect_path(&r.normalized()).and_then(|p| p.transform(e.world)) {
                pb.push_path(&inner);
            }
            if let Some(path) = pb.finish() {
                let fill = s.fill.unwrap_or(Color::rgba(0, 0, 0, 0x99));
                out.push(Op::Fill {
                    path: PathRef::Owned(path),
                    color: fill,
                    opacity: e.opacity,
                    rule: FillRule::EvenOdd,
                    space: Space::Board,
                    shadow: false,
                    multiply: false,
                });
            }
        }
        _ => {}
    }
    out
}
