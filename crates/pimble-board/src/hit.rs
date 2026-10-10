//! Precise hit testing: whether a board point touches what an element
//! actually paints (a diagonal stroke's line, not its bounding box's empty
//! corner; an unfilled shape's outline, not its middle; a frame's border and
//! name, not its interior), the topmost element under a point, and the
//! elements a marquee holds.

use crate::model::*;
use crate::paint::{callout_pointer, diamond_points, segment_parts};
use crate::render::ts_scale;
use crate::snapshot::{BoardSnapshot, Element, STEP_RADIUS};

/// Points around an ellipse when measuring the distance to its curve.
const ELLIPSE_STEPS: usize = 64;

fn dist(a: Point, b: Point) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// The distance from `p` to the segment `a`-`b`.
fn dist_to_segment(p: Point, a: Point, b: Point) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    if len2 <= f32::EPSILON {
        return dist(p, a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    dist(p, Point::new(a.x + t * dx, a.y + t * dy))
}

/// The distance from `p` to an open polyline.
fn dist_to_polyline(p: Point, pts: &[Point]) -> f32 {
    match pts {
        [] => f32::INFINITY,
        [only] => dist(p, *only),
        _ => pts.windows(2).map(|w| dist_to_segment(p, w[0], w[1])).fold(f32::INFINITY, f32::min),
    }
}

/// The distance from `p` to a closed polygon's outline.
fn dist_to_outline(p: Point, pts: &[Point]) -> f32 {
    let n = pts.len();
    (0..n).map(|i| dist_to_segment(p, pts[i], pts[(i + 1) % n])).fold(f32::INFINITY, f32::min)
}

/// Whether `p` is inside a polygon (even-odd).
fn inside_polygon(p: Point, pts: &[Point]) -> bool {
    let n = pts.len();
    let mut inside = false;
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (a, b) = (pts[i], pts[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Inside the polygon, or within `tol` of its outline.
fn near_polygon(p: Point, pts: &[Point], tol: f32) -> bool {
    inside_polygon(p, pts) || dist_to_outline(p, pts) <= tol
}

fn rect_corners(r: &Rect) -> [Point; 4] {
    [Point::new(r.x, r.y), Point::new(r.right(), r.y), Point::new(r.right(), r.bottom()), Point::new(r.x, r.bottom())]
}

fn ellipse_points(r: &Rect) -> Vec<Point> {
    let c = r.center();
    (0..ELLIPSE_STEPS)
        .map(|i| {
            let t = i as f32 / ELLIPSE_STEPS as f32 * std::f32::consts::TAU;
            Point::new(c.x + r.w / 2.0 * t.cos(), c.y + r.h / 2.0 * t.sin())
        })
        .collect()
}

/// A line's (or connector's) segment with its heads: within `reach` of the
/// shortened line or on a head.
fn hits_segment(s: &ResolvedStyle, a: Point, b: Point, p: Point, reach: f32, tol: f32) -> bool {
    let (heads, a2, b2) = segment_parts(s, a, b);
    dist_to_segment(p, a2, b2) <= reach || heads.iter().any(|tri| near_polygon(p, tri, tol))
}

impl Element {
    /// Whether board point `p` touches what this element paints, within
    /// `tolerance` board units (see the module doc for each kind's rule).
    /// Groups never hit (their members do), nor do spotlights, kinds this
    /// version does not know, and anything not drawn.
    pub fn hits(&self, p: Point, tolerance: f32) -> bool {
        if !self.visible || !self.has_extent || self.global {
            return false;
        }
        let s = &self.style;
        let tol = tolerance.max(0.0);
        if let Kind::Connector = self.raw.kind {
            let Some((a, b)) = self.route else { return false };
            if hits_segment(s, a, b, p, s.width / 2.0 + tol, tol) {
                return true;
            }
            return match (&self.layout, self.text_box) {
                (Some(l), Some(tb)) if !l.is_empty() => tb.inflate(s.font_size * 0.25 + tol).contains(p),
                _ => false,
            };
        }
        // Into element space; the tolerance scales with it (widths are
        // already element-space values).
        let Some(inv) = self.world.invert() else { return false };
        let mut q = [tiny_skia::Point::from_xy(p.x, p.y)];
        inv.map_points(&mut q);
        let q = Point::new(q[0].x, q[0].y);
        let tol = tol / ts_scale(&self.world).max(1e-6);
        let half = s.width / 2.0;
        match (&self.raw.kind, &self.raw.geom) {
            (Kind::Pen | Kind::Highlighter, _) => {
                let pts: Vec<Point> = self
                    .raw
                    .points
                    .iter()
                    .filter(|v| v[0].is_finite() && v[1].is_finite())
                    .map(|v| Point::new(v[0], v[1]))
                    .collect();
                dist_to_polyline(q, &pts) <= s.width.max(0.5) / 2.0 + tol
            }
            (Kind::Line | Kind::Arrow, Geom::Segment { a, b }) => hits_segment(s, *a, *b, q, half + tol, tol),
            (Kind::Rect | Kind::Ellipse | Kind::Diamond, Geom::Rect(r)) => {
                let r = r.normalized();
                let filled = s.fill.is_some() || self.raw.text.as_deref().is_some_and(|t| !t.trim().is_empty());
                let reach = half + tol;
                match self.raw.kind {
                    Kind::Ellipse => {
                        let c = r.center();
                        let (rx, ry) = ((r.w / 2.0).max(1e-3), (r.h / 2.0).max(1e-3));
                        let inside = ((q.x - c.x) / rx).powi(2) + ((q.y - c.y) / ry).powi(2) <= 1.0;
                        (filled && inside) || dist_to_outline(q, &ellipse_points(&r)) <= reach
                    }
                    Kind::Diamond => {
                        let pts = diamond_points(&r);
                        (filled && inside_polygon(q, &pts)) || dist_to_outline(q, &pts) <= reach
                    }
                    _ => {
                        let pts = rect_corners(&r);
                        (filled && r.contains(q)) || dist_to_outline(q, &pts) <= reach
                    }
                }
            }
            (Kind::Picture, Geom::Picture(pg)) => pg.rect.normalized().inflate(tol).contains(q),
            (Kind::Sticky, Geom::Rect(r)) => self.grown.unwrap_or(*r).normalized().inflate(tol).contains(q),
            (Kind::Card, Geom::Card { rect, .. }) => rect.normalized().inflate(tol).contains(q),
            (Kind::Callout, Geom::Callout { rect, tip }) => {
                let r = rect.normalized();
                r.inflate(half + tol).contains(q) || near_polygon(q, &callout_pointer(&r, *tip), half + tol)
            }
            (Kind::Step, Geom::Anchor(c)) => dist(q, *c) <= s.font_size * STEP_RADIUS + tol,
            (Kind::Text, Geom::Rect(_)) => self.text_box.is_some_and(|tb| tb.inflate(tol).contains(q)),
            (Kind::Frame, Geom::Rect(r)) => {
                let r = r.normalized();
                dist_to_outline(q, &rect_corners(&r)) <= s.width.max(0.5) / 2.0 + tol
                    || self.text_box.is_some_and(|tb| tb.inflate(tol).contains(q))
            }
            _ => false,
        }
    }
}

impl BoardSnapshot {
    /// The topmost element (last in z-order) that [`Element::hits`] `p`
    /// within `tolerance` board units.
    pub fn hit_precise(&self, p: Point, tolerance: f32) -> Option<&Element> {
        let t = tolerance.max(0.0);
        let r = Rect::new(p.x - t, p.y - t, 2.0 * t + 0.001, 2.0 * t + 0.001);
        self.query(&r).into_iter().rev().map(|i| &self.elements[i]).find(|e| e.hits(p, tolerance))
    }

    /// The elements a marquee over `rect` selects: visible, not global, not
    /// groups, with their whole bounds inside it; in z-order.
    pub fn within(&self, rect: &Rect) -> Vec<&Element> {
        let rect = rect.normalized();
        self.query(&rect)
            .into_iter()
            .map(|i| &self.elements[i])
            .filter(|e| {
                let b = e.bounds;
                !e.global
                    && e.has_extent
                    && !matches!(e.raw.kind, Kind::Group)
                    && b.x >= rect.x
                    && b.y >= rect.y
                    && b.right() <= rect.right()
                    && b.bottom() <= rect.bottom()
            })
            .collect()
    }

    /// Every frame, in z-order (the navigator's list).
    pub fn frames(&self) -> Vec<&Element> {
        self.elements.iter().filter(|e| matches!(e.raw.kind, Kind::Frame)).collect()
    }
}
