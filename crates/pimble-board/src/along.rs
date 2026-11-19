//! Text on a path (docs/DRAWING_WAVE6_CONTRACT.md section 4): a text
//! element whose style names another element in `along` is set along that
//! element's line, each glyph placed and turned to follow it; the text's own
//! box and transform are ignored while it does.
//!
//! The rules, all read from the document, nothing ever rewritten:
//!
//! - The element named must be a path, line, arrow or pen or highlighter
//!   stroke of the same canvas, with some length. Anything else (nothing, a
//!   shape, another text, the text itself) reads as plain text, the same on
//!   every device. A path's first subpath is the line followed.
//! - The line is the element's where it is now, through its world
//!   transform: moving or reshaping it moves the text.
//! - `along_offset` (0 to 1 of its length) is where the text sits, and the
//!   text's alignment says which part of it sits there: left, its start;
//!   center, its middle; right, its end. Absent, the alignment's own end
//!   (left 0, center 0.5, right 1).
//! - Text reads left to right: when the stretch it covers runs right to
//!   left, the line is read the other way (a river's name is never upside
//!   down), and `along_side` is judged in that reading direction: above, the
//!   baseline just above the line; on, centered on it; below, the capitals'
//!   tops just below it. Lines of a text with several stack away from it.
//! - Past either end, glyphs continue straight along the end's direction.

use std::hash::{Hash, Hasher};

use tiny_skia::{Path, PathBuilder, PathSegment, Transform as Affine};

use crate::model::*;
use crate::snapshot::{Element, Fx};
use crate::text::{align_offset, glyph_outline, vertical, TextLayout};

/// Refusal: "Put Text on Path" needs a text and a line.
pub const NEEDS_TEXT_AND_PATH: &str = "Select one text and one path, line or pen stroke to put the text on.";
/// Refusal: the element named cannot carry text.
pub const NOT_A_LINE: &str = "Text can be put only on a path, a line, an arrow or a pen stroke.";

/// Steps a curve is cut into when it is flattened.
const CURVE_STEPS: usize = 24;
/// The gap between a line and text above or below it, in em.
const GAP: f32 = 0.25;

/// Whether an element of `kind` can carry text along it.
pub fn takes_text(kind: &Kind) -> bool {
    matches!(kind, Kind::Path | Kind::Line | Kind::Arrow | Kind::Pen | Kind::Highlighter)
}

/// The line an element draws, flattened, in board units: a path's first
/// subpath, a line's segment, a stroke's points. `None` when it is not one
/// or has no length.
pub(crate) fn polyline_of(e: &Element) -> Option<Vec<Point>> {
    if !takes_text(&e.raw.kind) || !e.has_extent {
        return None;
    }
    let mut pts: Vec<Point> = match &e.raw.geom {
        Geom::Path(p) => {
            let path = p.to_tiny()?;
            let mut out: Vec<Point> = Vec::new();
            let mut start = None;
            for seg in path.segments() {
                let last = out.last().copied();
                match seg {
                    PathSegment::MoveTo(p) => {
                        if !out.is_empty() {
                            break;
                        }
                        out.push(Point::new(p.x, p.y));
                        start = Some(Point::new(p.x, p.y));
                    }
                    PathSegment::LineTo(p) => out.push(Point::new(p.x, p.y)),
                    PathSegment::QuadTo(a, p) => {
                        let Some(p0) = last else { continue };
                        for i in 1..=CURVE_STEPS {
                            let t = i as f32 / CURVE_STEPS as f32;
                            let u = 1.0 - t;
                            out.push(Point::new(u * u * p0.x + 2.0 * u * t * a.x + t * t * p.x, u * u * p0.y + 2.0 * u * t * a.y + t * t * p.y));
                        }
                    }
                    PathSegment::CubicTo(a, b, p) => {
                        let Some(p0) = last else { continue };
                        for i in 1..=CURVE_STEPS {
                            let t = i as f32 / CURVE_STEPS as f32;
                            let u = 1.0 - t;
                            let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                            out.push(Point::new(k0 * p0.x + k1 * a.x + k2 * b.x + k3 * p.x, k0 * p0.y + k1 * a.y + k2 * b.y + k3 * p.y));
                        }
                    }
                    PathSegment::Close => {
                        if let Some(s) = start {
                            out.push(s);
                        }
                        break;
                    }
                }
            }
            out
        }
        Geom::Segment { a, b } => vec![*a, *b],
        _ => e.raw.points.iter().filter(|p| p[0].is_finite() && p[1].is_finite()).map(|p| Point::new(p[0], p[1])).collect(),
    };
    if !e.world.is_identity() {
        let mut ts: Vec<tiny_skia::Point> = pts.iter().map(|p| tiny_skia::Point::from_xy(p.x, p.y)).collect();
        e.world.map_points(&mut ts);
        pts = ts.into_iter().map(|p| Point::new(p.x, p.y)).collect();
    }
    pts.dedup_by(|a, b| (a.x - b.x).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4);
    (pts.len() >= 2 && pts.iter().all(|p| p.x.is_finite() && p.y.is_finite())).then_some(pts)
}

/// A line with its cumulative lengths.
struct Measured {
    pts: Vec<Point>,
    /// `acc[i]` is the length from the start to `pts[i]`.
    acc: Vec<f32>,
}

impl Measured {
    fn new(pts: Vec<Point>) -> Self {
        let mut acc = Vec::with_capacity(pts.len());
        let mut d = 0.0;
        for (i, p) in pts.iter().enumerate() {
            if i > 0 {
                d += dist(pts[i - 1], *p);
            }
            acc.push(d);
        }
        Measured { pts, acc }
    }

    fn length(&self) -> f32 {
        *self.acc.last().unwrap_or(&0.0)
    }

    /// The point at distance `d` from the start and the direction there
    /// (unit); past an end, straight on along the end's direction.
    fn at(&self, d: f32) -> (Point, Point) {
        let n = self.pts.len();
        // The segment holding `d` (the first or last one past the ends).
        let i = match self.acc.iter().position(|a| *a >= d) {
            Some(0) => 0,
            Some(i) => i - 1,
            None => n - 2,
        }
        .min(n - 2);
        let (a, b) = (self.pts[i], self.pts[i + 1]);
        let len = (self.acc[i + 1] - self.acc[i]).max(1e-6);
        let t = (d - self.acc[i]) / len;
        let dir = Point::new((b.x - a.x) / len, (b.y - a.y) / len);
        (Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t), dir)
    }

    fn reversed(mut self) -> Self {
        self.pts.reverse();
        Measured::new(self.pts)
    }
}

fn dist(a: Point, b: Point) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// A text set along a line, in board units.
#[derive(Debug, Clone)]
pub struct Placed {
    /// The element it follows.
    pub target: String,
    /// The glyphs' outlines.
    pub path: Option<Path>,
    /// Each glyph's box (its advance by its ascender and descender), turned
    /// with it: corners in order. What hit testing reads.
    pub quads: Vec<[Point; 4]>,
    /// Everything it paints (before any halo).
    pub bounds: Rect,
    /// Changes whenever where the glyphs are does.
    pub hash: u64,
}

/// Set `layout` along `line` per `style` (see the module doc).
pub(crate) fn place(target: &str, layout: &TextLayout, style: &ResolvedStyle, line: Vec<Point>) -> Option<Placed> {
    let mut m = Measured::new(line);
    let total = m.length();
    if total <= 1e-3 || layout.is_empty() {
        return None;
    }
    let fs = layout.font_size;
    let w = layout.width;
    let fraction = style.along_offset.unwrap_or(match style.align {
        Align::Left => 0.0,
        Align::Center => 0.5,
        Align::Right => 1.0,
    });
    let anchor = fraction * total;
    let lead = |align: Align| match align {
        Align::Left => 0.0,
        Align::Center => w / 2.0,
        Align::Right => w,
    };
    let mut align = style.align;
    let mut start = anchor - lead(align);
    // Read left to right: a stretch running leftward is read from the
    // other end, and the alignment turns with it.
    let (a, _) = m.at(start);
    let (b, _) = m.at(start + w);
    if b.x < a.x - 1e-3 {
        m = m.reversed();
        align = match align {
            Align::Left => Align::Right,
            Align::Right => Align::Left,
            Align::Center => Align::Center,
        };
        start = (total - anchor) - lead(align);
    }
    let (ascender, descender, cap) = vertical(fs);
    let lh = layout.line_height();
    let n = layout.lines.len();
    // How far each line's baseline sits from the line, toward "above".
    let lift = |k: usize| -> f32 {
        match style.along_side {
            AlongSide::Above => GAP * fs + (n - 1 - k) as f32 * lh,
            AlongSide::On => -cap / 2.0 + ((n - 1) as f32 / 2.0 - k as f32) * lh,
            AlongSide::Below => -(cap + GAP * fs) - k as f32 * lh,
        }
    };
    let glyph = layout.glyph_matrix();
    let mut builder = PathBuilder::new();
    let mut quads = Vec::new();
    let mut h = Fx::default();
    for (k, l) in layout.lines.iter().enumerate() {
        let line_start = start + align_offset(align, w, l.width);
        let up = lift(k);
        for g in &l.glyphs {
            let c = line_start + g.x + g.adv / 2.0;
            let (p, t) = m.at(c);
            // "Above" is to the left of the direction of travel (y down).
            let nrm = Point::new(t.y, -t.x);
            let o = Point::new(p.x + nrm.x * up, p.y + nrm.y * up);
            let turn = Affine::from_row(t.x, t.y, -t.y, t.x, o.x, o.y).pre_concat(Affine::from_translate(-g.adv / 2.0, 0.0));
            if !g.space {
                if let Some(outline) = glyph_outline(g.face, g.id) {
                    if let Some(placed) = (*outline).clone().transform(turn.pre_concat(glyph)) {
                        builder.push_path(&placed);
                    }
                }
            }
            let mut corners = [
                tiny_skia::Point::from_xy(0.0, -ascender),
                tiny_skia::Point::from_xy(g.adv, -ascender),
                tiny_skia::Point::from_xy(g.adv, descender),
                tiny_skia::Point::from_xy(0.0, descender),
            ];
            turn.map_points(&mut corners);
            let quad = corners.map(|q| Point::new(q.x, q.y));
            for q in &quad {
                q.x.to_bits().hash(&mut h);
                q.y.to_bits().hash(&mut h);
            }
            quads.push(quad);
        }
    }
    let path = builder.finish();
    let mut bounds: Option<Rect> = path.as_ref().map(|p| {
        let b = p.bounds();
        Rect::new(b.left(), b.top(), b.width(), b.height())
    });
    for q in &quads {
        for p in q {
            let r = Rect::new(p.x, p.y, 0.0, 0.0);
            bounds = Some(bounds.map_or(r, |b| b.union(&r)));
        }
    }
    target.hash(&mut h);
    Some(Placed { target: target.to_string(), path, quads, bounds: bounds?, hash: h.finish() })
}
