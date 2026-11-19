//! Clip masks (docs/DRAWING_WAVE6_CONTRACT.md section 1): a layer or group
//! names a closed shape or closed path of the same canvas in its `clip` key,
//! and everything it draws, vector and paint, shows only inside that
//! element's outline. The clip element itself draws where it is (unless its
//! own layer is hidden: the usual "Clips" layer).
//!
//! The rules, all read from the document, nothing ever rewritten:
//!
//! - A clip that names no element, an element that is not closed (a line, a
//!   stroke, an open path, text, a picture...), or an element drawn inside
//!   the very layer or group it would clip (a cycle) reads as no clip, the
//!   same on every device.
//! - The clip is the element's outline where it is now, through its world
//!   transform: moving or reshaping it moves the edge. Paint is kept whole
//!   and clipped when its layer's buffer is blended down.
//! - A clipped layer inside a clipped group shows only inside both.

use std::hash::{Hash, Hasher};

use tiny_skia::{FillRule, Mask, Path, PathSegment, Transform as Affine};

use crate::model::*;
use crate::paint::{rect_path, shape_path};
use crate::snapshot::Element;

/// Refusal: the element named is not a closed shape or a closed path.
pub const NOT_CLOSED: &str =
    "Only a closed shape or a closed path can clip a layer: a rectangle, an ellipse, a polygon, a star, a frame, or a path whose ends are joined.";
/// Refusal: the element named would clip the layer it is in.
pub const CLIP_CYCLE: &str =
    "That shape is inside the layer it would clip. Move it to another layer first (a hidden \"Clips\" layer is the usual place).";

/// Whether an element of `kind` with `geom` can be a clip: a closed shape
/// (rectangle, ellipse, diamond, polygon, star, frame) or a path whose every
/// subpath is closed.
pub fn is_closed(kind: &Kind, geom: &Geom) -> bool {
    match (kind, geom) {
        (Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Polygon | Kind::Star | Kind::Frame, Geom::Rect(_)) => true,
        (Kind::Path, Geom::Path(p)) => !p.subpaths.is_empty() && p.subpaths.iter().all(|s| s.closed && s.anchors.len() >= 2),
        _ => false,
    }
}

/// A clip resolved for drawing: the element's outline in board space.
#[derive(Debug)]
pub struct Clip {
    /// The element that clips.
    pub element: String,
    /// Its outline, board units.
    pub path: Path,
    pub rule: FillRule,
    /// The outline's bounds, board units.
    pub bounds: Rect,
    /// The outline flattened into polygons, for hit testing.
    polys: Vec<Vec<Point>>,
    /// Changes whenever the outline does.
    pub hash: u64,
}

/// Steps a curve is cut into when flattened for hit testing.
const CURVE_STEPS: usize = 16;

impl Clip {
    /// The clip element `e` makes, when it is closed.
    pub(crate) fn of(e: &Element) -> Option<Clip> {
        if !is_closed(&e.raw.kind, &e.raw.geom) {
            return None;
        }
        let (local, rule) = match (&e.raw.kind, &e.raw.geom) {
            (Kind::Path, Geom::Path(p)) => (p.to_tiny()?, p.rule.to_tiny()),
            (Kind::Frame, Geom::Rect(r)) => (rect_path(&r.normalized())?, FillRule::Winding),
            (kind, Geom::Rect(r)) => (shape_path(kind, &r.normalized(), &e.style)?, FillRule::Winding),
            _ => return None,
        };
        let path = local.transform(e.world)?;
        let b = path.bounds();
        let bounds = Rect::new(b.x(), b.y(), b.width(), b.height());
        let polys = flatten(&path);
        let mut h = crate::snapshot::Fx::default();
        e.raw.id.hash(&mut h);
        (rule == FillRule::EvenOdd).hash(&mut h);
        for p in path.points() {
            p.x.to_bits().hash(&mut h);
            p.y.to_bits().hash(&mut h);
        }
        path.verbs().len().hash(&mut h);
        Some(Clip { element: e.raw.id.clone(), path, rule, bounds, polys, hash: h.finish() })
    }

    /// The outline of closed element `e` where it is drawn (what Scatter in
    /// Shape fills): `None` when it is not closed.
    pub fn of_element(e: &Element) -> Option<Clip> {
        Clip::of(e)
    }

    /// An outline given as a path in board units (an SVG `d` read by the
    /// caller).
    pub fn of_path(path: Path, rule: FillRule) -> Option<Clip> {
        let b = path.bounds();
        let bounds = Rect::new(b.x(), b.y(), b.width(), b.height());
        if !(bounds.w > 0.0 && bounds.h > 0.0) {
            return None;
        }
        let polys = flatten(&path);
        let mut h = crate::snapshot::Fx::default();
        for p in path.points() {
            p.x.to_bits().hash(&mut h);
            p.y.to_bits().hash(&mut h);
        }
        Some(Clip { element: String::new(), path, rule, bounds, polys, hash: h.finish() })
    }

    /// Whether board point `p` is inside the outline (by its fill rule).
    pub fn contains(&self, p: Point) -> bool {
        if !self.bounds.inflate(1e-3).contains(p) {
            return false;
        }
        let mut winding = 0i32;
        for poly in &self.polys {
            let n = poly.len();
            for i in 0..n {
                let (a, b) = (poly[i], poly[(i + 1) % n]);
                if a.y <= p.y {
                    if b.y > p.y && cross(a, b, p) > 0.0 {
                        winding += 1;
                    }
                } else if b.y <= p.y && cross(a, b, p) < 0.0 {
                    winding -= 1;
                }
            }
        }
        match self.rule {
            FillRule::Winding => winding != 0,
            FillRule::EvenOdd => winding % 2 != 0,
        }
    }

    /// The outline as a coverage mask of a `width` x `height` target drawn
    /// with `vt` (board to pixels). The size of the target, never of the
    /// world: a tile's, or the visible area's.
    pub(crate) fn mask(&self, width: u32, height: u32, vt: Affine) -> Option<Mask> {
        let mut mask = Mask::new(width, height)?;
        mask.fill_path(&self.path, self.rule, true, vt);
        Some(mask)
    }
}

fn cross(a: Point, b: Point, p: Point) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y)
}

/// A path's outline as closed polygons (every subpath closed, as a fill
/// closes it).
fn flatten(path: &Path) -> Vec<Vec<Point>> {
    let mut out: Vec<Vec<Point>> = Vec::new();
    let mut cur: Vec<Point> = Vec::new();
    let mut last = Point::new(0.0, 0.0);
    let flush = |cur: &mut Vec<Point>, out: &mut Vec<Vec<Point>>| {
        if cur.len() >= 3 {
            out.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                flush(&mut cur, &mut out);
                last = Point::new(p.x, p.y);
                cur.push(last);
            }
            PathSegment::LineTo(p) => {
                last = Point::new(p.x, p.y);
                cur.push(last);
            }
            PathSegment::QuadTo(c, p) => {
                for k in 1..=CURVE_STEPS {
                    let t = k as f32 / CURVE_STEPS as f32;
                    let u = 1.0 - t;
                    cur.push(Point::new(u * u * last.x + 2.0 * u * t * c.x + t * t * p.x, u * u * last.y + 2.0 * u * t * c.y + t * t * p.y));
                }
                last = Point::new(p.x, p.y);
            }
            PathSegment::CubicTo(c1, c2, p) => {
                for k in 1..=CURVE_STEPS {
                    let t = k as f32 / CURVE_STEPS as f32;
                    let u = 1.0 - t;
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    cur.push(Point::new(
                        a * last.x + b * c1.x + c * c2.x + d * p.x,
                        a * last.y + b * c1.y + c * c2.y + d * p.y,
                    ));
                }
                last = Point::new(p.x, p.y);
            }
            PathSegment::Close => flush(&mut cur, &mut out),
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Why a layer's clip naming element `named` reads as no clip, in a few
/// words (for the layers panel and the MCP).
pub fn unmet(snapshot: &crate::snapshot::BoardSnapshot, named: &str) -> &'static str {
    match snapshot.get(named) {
        None => "its shape is gone",
        Some(e) if !is_closed(&e.raw.kind, &e.raw.geom) => "its shape is not closed",
        Some(_) => "its shape is inside the layer it clips",
    }
}

/// How a layer's clip element is named to a person: its note's first line
/// when it has one, else its kind and the start of its id.
pub fn describe(snapshot: &crate::snapshot::BoardSnapshot, id: &str) -> String {
    let short: String = id.chars().take(8).collect();
    match snapshot.get(id) {
        Some(e) => match e.raw.note.as_deref().and_then(|n| n.lines().next()).map(str::trim).filter(|n| !n.is_empty()) {
            Some(note) => format!("\"{note}\""),
            None => format!("the {} {short}", e.raw.kind.as_str()),
        },
        None => format!("element {short}"),
    }
}
