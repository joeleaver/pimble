//! Paths (docs/DRAWING_CONTRACT.md section 2, "The path element"): one or
//! more subpaths of cubic Bézier anchors, and the pure helpers the Pen,
//! Direct select and Pencil tools edit them with.
//!
//! ```text
//! geom: Any {
//!   subpaths: [ { closed: bool, anchors: [x, y, in_dx, in_dy, out_dx, out_dy, mode, ...] } ],
//!   rule: "nonzero" | "evenodd",
//! }
//! ```
//!
//! An anchor is a point with two handles, each stored as an offset from the
//! point (so moving an anchor moves its handles), and a mode: corner (the
//! handles are independent), smooth (collinear), symmetric (collinear and
//! the same length). `mode` is stored as a number (0 corner, 1 smooth, 2
//! symmetric); a reader also takes the names. Segment `i` of a subpath runs
//! from anchor `i` to anchor `i + 1` (and, closed, the last one back to the
//! first): its cubic is `p_i, p_i + out_i, p_{i+1} + in_{i+1}, p_{i+1}`. A
//! segment whose two handles are zero is a straight line.
//!
//! The geometry is one value, last writer wins, like every element's: every
//! helper here edits a [`PathGeom`] in memory, and the caller writes it
//! whole with [`crate::BoardTxn::set_geom`].
//!
//! Also here: [`fit`] (the Pencil: freehand points fitted to Béziers,
//! Schneider's algorithm), [`to_d`] and [`parse_d`] (SVG path data), the
//! live shapes as paths ([`rect`], [`ellipse`], [`diamond`], [`polygon`],
//! [`star`]) and [`to_path`], which turns an element into one.

use std::fmt::Write as _;

use crate::model::*;

/// How an anchor's two handles are tied together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AnchorMode {
    /// Each handle moves alone (a sharp turn, or a straight end).
    #[default]
    Corner,
    /// The handles stay collinear; each keeps its own length.
    Smooth,
    /// The handles stay collinear and the same length.
    Symmetric,
}

impl AnchorMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AnchorMode::Corner => "corner",
            AnchorMode::Smooth => "smooth",
            AnchorMode::Symmetric => "symmetric",
        }
    }

    pub fn parse(s: &str) -> Option<AnchorMode> {
        Some(match s {
            "corner" => AnchorMode::Corner,
            "smooth" => AnchorMode::Smooth,
            "symmetric" => AnchorMode::Symmetric,
            _ => return None,
        })
    }

    /// The number stored in the anchor array.
    pub fn code(&self) -> u8 {
        match self {
            AnchorMode::Corner => 0,
            AnchorMode::Smooth => 1,
            AnchorMode::Symmetric => 2,
        }
    }

    /// From the stored number: anything unknown reads as corner.
    pub fn from_code(code: f64) -> AnchorMode {
        match code.round() as i64 {
            1 => AnchorMode::Smooth,
            2 => AnchorMode::Symmetric,
            _ => AnchorMode::Corner,
        }
    }
}

/// One anchor: its point, its handles as offsets from the point, its mode.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Anchor {
    pub p: Point,
    /// The incoming handle, relative to `p`.
    pub h_in: Point,
    /// The outgoing handle, relative to `p`.
    pub h_out: Point,
    pub mode: AnchorMode,
}

impl Anchor {
    pub fn new(p: Point, h_in: Point, h_out: Point, mode: AnchorMode) -> Self {
        Self { p, h_in, h_out, mode }
    }

    /// A corner with no handles.
    pub fn corner(p: Point) -> Self {
        Self { p, ..Self::default() }
    }

    /// Where the incoming handle's tip is, absolute.
    pub fn in_point(&self) -> Point {
        add(self.p, self.h_in)
    }

    /// Where the outgoing handle's tip is, absolute.
    pub fn out_point(&self) -> Point {
        add(self.p, self.h_out)
    }

    /// The handle on `side`, relative.
    pub fn handle(&self, side: HandleSide) -> Point {
        match side {
            HandleSide::In => self.h_in,
            HandleSide::Out => self.h_out,
        }
    }
}

/// A run of anchors, open or closed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Subpath {
    pub closed: bool,
    pub anchors: Vec<Anchor>,
}

impl Subpath {
    pub fn new(anchors: Vec<Anchor>, closed: bool) -> Self {
        Self { closed, anchors }
    }

    /// How many segments it has: one fewer than its anchors, or as many
    /// when closed; none under two anchors.
    pub fn segment_count(&self) -> usize {
        let n = self.anchors.len();
        if n < 2 {
            0
        } else if self.closed {
            n
        } else {
            n - 1
        }
    }

    /// The anchor index segment `i` ends at.
    fn next(&self, i: usize) -> usize {
        (i + 1) % self.anchors.len()
    }

    /// Segment `i`'s cubic: start, two controls, end (absolute).
    pub fn segment(&self, i: usize) -> Option<[Point; 4]> {
        if i >= self.segment_count() {
            return None;
        }
        let a = &self.anchors[i];
        let b = &self.anchors[self.next(i)];
        Some([a.p, a.out_point(), b.in_point(), b.p])
    }

    /// Whether segment `i` is a straight line (both its handles zero).
    pub fn is_line(&self, i: usize) -> bool {
        let a = &self.anchors[i];
        let b = &self.anchors[self.next(i)];
        is_zero(a.h_out) && is_zero(b.h_in)
    }
}

/// How overlapping subpaths fill (holes): SVG's `fill-rule`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PathRule {
    #[default]
    NonZero,
    EvenOdd,
}

impl PathRule {
    pub fn as_str(&self) -> &'static str {
        match self {
            PathRule::NonZero => "nonzero",
            PathRule::EvenOdd => "evenodd",
        }
    }

    pub fn parse(s: &str) -> Option<PathRule> {
        Some(match s {
            "nonzero" => PathRule::NonZero,
            "evenodd" => PathRule::EvenOdd,
            _ => return None,
        })
    }

    pub fn to_tiny(&self) -> tiny_skia::FillRule {
        match self {
            PathRule::NonZero => tiny_skia::FillRule::Winding,
            PathRule::EvenOdd => tiny_skia::FillRule::EvenOdd,
        }
    }
}

/// A path element's geometry: its subpaths and fill rule.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PathGeom {
    pub subpaths: Vec<Subpath>,
    pub rule: PathRule,
}

/// An anchor, by subpath and index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnchorRef {
    pub subpath: usize,
    pub index: usize,
}

impl AnchorRef {
    pub fn new(subpath: usize, index: usize) -> Self {
        Self { subpath, index }
    }
}

/// Which of an anchor's handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandleSide {
    In,
    Out,
}

/// A handle, by its anchor and side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HandleRef {
    pub anchor: AnchorRef,
    pub side: HandleSide,
}

/// Where a point met a segment: which one, at which parameter, the point on
/// the curve, and how far away it was.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentHit {
    pub subpath: usize,
    /// The segment (it starts at this anchor index).
    pub segment: usize,
    /// The cubic's parameter there, 0 to 1.
    pub t: f32,
    pub point: Point,
    pub distance: f32,
}

/// Why SVG path data was refused.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PathError {
    #[error("the path data has a mistake at character {at}: {message}")]
    Syntax { at: usize, message: String },
    #[error("the path data draws nothing")]
    Empty,
}

// ── Small vector helpers ────────────────────────────────────────────────────

fn add(a: Point, b: Point) -> Point {
    Point::new(a.x + b.x, a.y + b.y)
}

fn sub(a: Point, b: Point) -> Point {
    Point::new(a.x - b.x, a.y - b.y)
}

fn mul(a: Point, k: f32) -> Point {
    Point::new(a.x * k, a.y * k)
}

fn len(a: Point) -> f32 {
    (a.x * a.x + a.y * a.y).sqrt()
}

fn dist(a: Point, b: Point) -> f32 {
    len(sub(a, b))
}

fn is_zero(a: Point) -> bool {
    a.x == 0.0 && a.y == 0.0
}

fn lerp(a: Point, b: Point, t: f32) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn normalized(a: Point) -> Option<Point> {
    let l = len(a);
    (l > 1e-9).then(|| mul(a, 1.0 / l))
}

/// The point at `t` on a cubic.
pub fn cubic_at(c: &[Point; 4], t: f32) -> Point {
    let mt = 1.0 - t;
    let (a, b, cc, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
    Point::new(a * c[0].x + b * c[1].x + cc * c[2].x + d * c[3].x, a * c[0].y + b * c[1].y + cc * c[2].y + d * c[3].y)
}

/// A cubic split at `t` (de Casteljau): the two halves, which together are
/// the same curve.
pub fn split_cubic(c: &[Point; 4], t: f32) -> ([Point; 4], [Point; 4]) {
    let p01 = lerp(c[0], c[1], t);
    let p12 = lerp(c[1], c[2], t);
    let p23 = lerp(c[2], c[3], t);
    let p012 = lerp(p01, p12, t);
    let p123 = lerp(p12, p23, t);
    let m = lerp(p012, p123, t);
    ([c[0], p01, p012, m], [m, p123, p23, c[3]])
}

/// The parameters in (0, 1) where one coordinate of a cubic turns.
fn cubic_extrema(a: f32, b: f32, c: f32, d: f32) -> Vec<f32> {
    // Derivative / 3: (b-a)(1-t)^2 + 2(c-b)(1-t)t + (d-c)t^2 = A t^2 + B t + C.
    let (p, q, r) = (b - a, c - b, d - c);
    let qa = p - 2.0 * q + r;
    let qb = 2.0 * (q - p);
    let qc = p;
    let mut out = Vec::new();
    if qa.abs() < 1e-9 {
        if qb.abs() > 1e-9 {
            out.push(-qc / qb);
        }
    } else {
        let disc = qb * qb - 4.0 * qa * qc;
        if disc >= 0.0 {
            let s = disc.sqrt();
            out.push((-qb + s) / (2.0 * qa));
            out.push((-qb - s) / (2.0 * qa));
        }
    }
    out.retain(|t| *t > 0.0 && *t < 1.0);
    out
}

fn cubic_bounds(c: &[Point; 4]) -> Rect {
    let mut r = Rect::from_points(c[0], c[3]);
    let mut ts = cubic_extrema(c[0].x, c[1].x, c[2].x, c[3].x);
    ts.extend(cubic_extrema(c[0].y, c[1].y, c[2].y, c[3].y));
    for t in ts {
        let p = cubic_at(c, t);
        r = r.union(&Rect::new(p.x, p.y, 0.0, 0.0));
    }
    r
}

/// How many straight pieces a cubic is cut into to stay within
/// `tolerance` of it.
fn pieces(c: &[Point; 4], tolerance: f32) -> usize {
    let ctrl = dist(c[0], c[1]) + dist(c[1], c[2]) + dist(c[2], c[3]);
    let chord = dist(c[0], c[3]);
    if ctrl - chord < 1e-4 {
        return 1;
    }
    (((ctrl / tolerance.max(1e-3)).sqrt() * 1.5).ceil() as usize).clamp(2, 256)
}

/// The closest point on cubic `c` to `p`: (distance, t, point).
fn closest_on_cubic(c: &[Point; 4], p: Point) -> (f32, f32, Point) {
    const N: usize = 48;
    let mut best = (f32::INFINITY, 0.0f32);
    for i in 0..=N {
        let t = i as f32 / N as f32;
        let d = dist(cubic_at(c, t), p);
        if d < best.0 {
            best = (d, t);
        }
    }
    // Golden-section refinement around the best sample.
    let (mut lo, mut hi) = ((best.1 - 1.0 / N as f32).max(0.0), (best.1 + 1.0 / N as f32).min(1.0));
    let g = 0.618_034f32;
    for _ in 0..30 {
        let m1 = hi - g * (hi - lo);
        let m2 = lo + g * (hi - lo);
        if dist(cubic_at(c, m1), p) < dist(cubic_at(c, m2), p) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    let t = (lo + hi) / 2.0;
    let q = cubic_at(c, t);
    let d = dist(q, p);
    if d <= best.0 {
        (d, t, q)
    } else {
        (best.0, best.1, cubic_at(c, best.1))
    }
}

/// The mode two handles have between them: smooth when collinear and
/// opposite, symmetric when also the same length, else corner.
pub fn infer_mode(h_in: Point, h_out: Point) -> AnchorMode {
    let (li, lo) = (len(h_in), len(h_out));
    if li < 1e-6 || lo < 1e-6 {
        return AnchorMode::Corner;
    }
    let cross = (h_in.x * h_out.y - h_in.y * h_out.x) / (li * lo);
    let dot = (h_in.x * h_out.x + h_in.y * h_out.y) / (li * lo);
    if cross.abs() > 1e-3 || dot > -0.999 {
        return AnchorMode::Corner;
    }
    if (li - lo).abs() <= 1e-3 * li.max(lo).max(1.0) {
        AnchorMode::Symmetric
    } else {
        AnchorMode::Smooth
    }
}

impl PathGeom {
    pub fn new(subpaths: Vec<Subpath>, rule: PathRule) -> Self {
        Self { subpaths, rule }
    }

    /// One subpath through `anchors`.
    pub fn from_anchors(anchors: Vec<Anchor>, closed: bool) -> Self {
        Self { subpaths: vec![Subpath::new(anchors, closed)], rule: PathRule::NonZero }
    }

    /// One subpath of corners through `points` (straight segments).
    pub fn polyline(points: &[Point], closed: bool) -> Self {
        Self::from_anchors(points.iter().map(|p| Anchor::corner(*p)).collect(), closed)
    }

    /// No subpaths: nothing to draw (an element left like this should be
    /// deleted).
    pub fn is_empty(&self) -> bool {
        self.subpaths.is_empty()
    }

    pub fn anchor_count(&self) -> usize {
        self.subpaths.iter().map(|s| s.anchors.len()).sum()
    }

    pub fn anchor(&self, at: AnchorRef) -> Option<&Anchor> {
        self.subpaths.get(at.subpath)?.anchors.get(at.index)
    }

    pub fn anchor_mut(&mut self, at: AnchorRef) -> Option<&mut Anchor> {
        self.subpaths.get_mut(at.subpath)?.anchors.get_mut(at.index)
    }

    /// Every anchor's reference, subpath by subpath.
    pub fn anchor_refs(&self) -> Vec<AnchorRef> {
        self.subpaths.iter().enumerate().flat_map(|(s, sp)| (0..sp.anchors.len()).map(move |i| AnchorRef::new(s, i))).collect()
    }

    /// Every segment: (subpath, segment, its cubic).
    pub fn segments(&self) -> impl Iterator<Item = (usize, usize, [Point; 4])> + '_ {
        self.subpaths
            .iter()
            .enumerate()
            .flat_map(|(s, sp)| (0..sp.segment_count()).filter_map(move |i| sp.segment(i).map(|c| (s, i, c))))
    }

    /// The exact extent of its curves (handles not included); `None` when
    /// it has no anchors.
    pub fn bounds(&self) -> Option<Rect> {
        let mut acc: Option<Rect> = None;
        for sp in &self.subpaths {
            for a in &sp.anchors {
                let r = Rect::new(a.p.x, a.p.y, 0.0, 0.0);
                acc = Some(acc.map_or(r, |b| b.union(&r)));
            }
        }
        for (_, _, c) in self.segments() {
            let r = cubic_bounds(&c);
            acc = Some(acc.map_or(r, |b| b.union(&r)));
        }
        acc.filter(|r| r.is_finite())
    }

    /// Each subpath as straight pieces within `tolerance` of its curves:
    /// (points, closed). A closed subpath's points do not repeat the first.
    pub fn flatten(&self, tolerance: f32) -> Vec<(Vec<Point>, bool)> {
        self.subpaths
            .iter()
            .filter(|sp| !sp.anchors.is_empty())
            .map(|sp| {
                let mut pts = vec![sp.anchors[0].p];
                for i in 0..sp.segment_count() {
                    let c = sp.segment(i).unwrap();
                    let n = if sp.is_line(i) { 1 } else { pieces(&c, tolerance) };
                    for k in 1..=n {
                        pts.push(cubic_at(&c, k as f32 / n as f32));
                    }
                }
                if sp.closed && pts.len() > 1 {
                    pts.pop();
                }
                (pts, sp.closed)
            })
            .collect()
    }

    /// As tiny-skia's path (`None` when it draws nothing). A subpath of one
    /// anchor is a dot (a zero-length line, which a round cap draws).
    pub fn to_tiny(&self) -> Option<tiny_skia::Path> {
        let mut pb = tiny_skia::PathBuilder::new();
        for sp in &self.subpaths {
            let Some(first) = sp.anchors.first() else { continue };
            pb.move_to(first.p.x, first.p.y);
            if sp.anchors.len() == 1 {
                pb.line_to(first.p.x + 0.01, first.p.y);
                continue;
            }
            for i in 0..sp.segment_count() {
                let c = sp.segment(i).unwrap();
                if sp.is_line(i) {
                    pb.line_to(c[3].x, c[3].y);
                } else {
                    pb.cubic_to(c[1].x, c[1].y, c[2].x, c[2].y, c[3].x, c[3].y);
                }
            }
            if sp.closed {
                pb.close();
            }
        }
        pb.finish()
    }

    /// Every point mapped by `f` (an affine map), handles as `f(p + h) - f(p)`.
    pub fn map(&self, f: impl Fn(Point) -> Point) -> PathGeom {
        PathGeom {
            rule: self.rule,
            subpaths: self
                .subpaths
                .iter()
                .map(|sp| Subpath {
                    closed: sp.closed,
                    anchors: sp
                        .anchors
                        .iter()
                        .map(|a| {
                            let p = f(a.p);
                            Anchor { p, h_in: sub(f(a.in_point()), p), h_out: sub(f(a.out_point()), p), mode: a.mode }
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn translated(&self, dx: f32, dy: f32) -> PathGeom {
        self.map(|p| Point::new(p.x + dx, p.y + dy))
    }

    /// Scaled so its box `from` becomes `to` (a geometric resize).
    pub fn rescaled(&self, from: &Rect, to: &Rect) -> PathGeom {
        let kx = to.w / from.w.max(1e-6);
        let ky = to.h / from.h.max(1e-6);
        self.map(|p| Point::new(to.x + (p.x - from.x) * kx, to.y + (p.y - from.y) * ky))
    }

    // ── Hit testing ──────────────────────────────────────────────────────

    /// The anchor nearest `p` within `tolerance`.
    pub fn hit_anchor(&self, p: Point, tolerance: f32) -> Option<AnchorRef> {
        let mut best: Option<(f32, AnchorRef)> = None;
        for (s, sp) in self.subpaths.iter().enumerate() {
            for (i, a) in sp.anchors.iter().enumerate() {
                let d = dist(a.p, p);
                if d <= tolerance && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, AnchorRef::new(s, i)));
                }
            }
        }
        best.map(|(_, r)| r)
    }

    /// The handle tip nearest `p` within `tolerance` (handles of zero
    /// length are not there to hit). With `only`, only those anchors'
    /// handles (a tool shows the selected anchors' handles alone).
    pub fn hit_handle(&self, p: Point, tolerance: f32, only: Option<&[AnchorRef]>) -> Option<HandleRef> {
        let mut best: Option<(f32, HandleRef)> = None;
        for (s, sp) in self.subpaths.iter().enumerate() {
            for (i, a) in sp.anchors.iter().enumerate() {
                let at = AnchorRef::new(s, i);
                if only.is_some_and(|o| !o.contains(&at)) {
                    continue;
                }
                for side in [HandleSide::In, HandleSide::Out] {
                    let h = a.handle(side);
                    if is_zero(h) {
                        continue;
                    }
                    let d = dist(add(a.p, h), p);
                    if d <= tolerance && best.is_none_or(|(b, _)| d < b) {
                        best = Some((d, HandleRef { anchor: at, side }));
                    }
                }
            }
        }
        best.map(|(_, r)| r)
    }

    /// The segment nearest `p` within `tolerance`, with the parameter of the
    /// nearest point on it (what [`PathGeom::insert_anchor`] takes).
    pub fn hit_segment(&self, p: Point, tolerance: f32) -> Option<SegmentHit> {
        let mut best: Option<SegmentHit> = None;
        for (s, i, c) in self.segments() {
            let r = cubic_bounds(&c).inflate(tolerance);
            if !r.contains(p) {
                continue;
            }
            let (d, t, q) = closest_on_cubic(&c, p);
            if d <= tolerance && best.is_none_or(|b| d < b.distance) {
                best = Some(SegmentHit { subpath: s, segment: i, t, point: q, distance: d });
            }
        }
        best
    }

    /// The distance from `p` to its outline (an open subpath has no closing
    /// segment).
    pub fn distance_to(&self, p: Point) -> f32 {
        let mut best = f32::INFINITY;
        for sp in &self.subpaths {
            if sp.anchors.len() == 1 {
                best = best.min(dist(sp.anchors[0].p, p));
            }
        }
        for (_, _, c) in self.segments() {
            best = best.min(closest_on_cubic(&c, p).0);
        }
        best
    }

    /// Whether `p` is inside its fill, by its rule (an open subpath fills as
    /// if closed, as SVG fills it).
    pub fn contains(&self, p: Point) -> bool {
        let mut winding = 0i32;
        let mut crossings = 0u32;
        for (pts, _) in self.flatten(0.25) {
            let n = pts.len();
            if n < 3 {
                continue;
            }
            for i in 0..n {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                if (a.y <= p.y) != (b.y <= p.y) {
                    let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
                    if p.x < x {
                        crossings += 1;
                        winding += if b.y > a.y { 1 } else { -1 };
                    }
                }
            }
        }
        match self.rule {
            PathRule::NonZero => winding != 0,
            PathRule::EvenOdd => crossings % 2 == 1,
        }
    }

    // ── Editing ──────────────────────────────────────────────────────────

    /// A new anchor on a segment at parameter `t` (de Casteljau): the curve
    /// keeps its shape, the neighbors' handles shorten to match. Answers the
    /// new anchor; `None` when the segment does not exist or `t` is at an
    /// end.
    pub fn insert_anchor(&mut self, subpath: usize, segment: usize, t: f32) -> Option<AnchorRef> {
        if !(t > 1e-4 && t < 1.0 - 1e-4) {
            return None;
        }
        let sp = self.subpaths.get_mut(subpath)?;
        let c = sp.segment(segment)?;
        let line = sp.is_line(segment);
        let next = sp.next(segment);
        let (l, r) = split_cubic(&c, t);
        let m = l[3];
        let new = if line {
            Anchor::corner(m)
        } else {
            Anchor { p: m, h_in: sub(l[2], m), h_out: sub(r[1], m), mode: AnchorMode::Smooth }
        };
        if !line {
            let a = &mut sp.anchors[segment];
            a.h_out = sub(l[1], l[0]);
            if a.mode == AnchorMode::Symmetric {
                a.mode = AnchorMode::Smooth;
            }
            let b = &mut sp.anchors[next];
            b.h_in = sub(r[2], r[3]);
            if b.mode == AnchorMode::Symmetric {
                b.mode = AnchorMode::Smooth;
            }
        }
        let at = segment + 1;
        sp.anchors.insert(at, new);
        Some(AnchorRef::new(subpath, at))
    }

    /// Remove anchors. A subpath left with fewer than two anchors goes.
    /// Answers whether the path is now empty (the caller deletes the
    /// element then). References that name nothing are ignored.
    pub fn delete_anchors(&mut self, refs: &[AnchorRef]) -> bool {
        let mut refs: Vec<AnchorRef> = refs.to_vec();
        refs.sort_unstable();
        refs.dedup();
        for r in refs.iter().rev() {
            if let Some(sp) = self.subpaths.get_mut(r.subpath) {
                if r.index < sp.anchors.len() {
                    sp.anchors.remove(r.index);
                }
            }
        }
        self.subpaths.retain(|sp| sp.anchors.len() >= 2);
        self.is_empty()
    }

    /// The neighbors of an anchor along its subpath (none past an open
    /// end).
    fn neighbors(&self, at: AnchorRef) -> (Option<Point>, Option<Point>) {
        let Some(sp) = self.subpaths.get(at.subpath) else { return (None, None) };
        let n = sp.anchors.len();
        if n < 2 {
            return (None, None);
        }
        let prev = if at.index > 0 { Some(at.index - 1) } else if sp.closed { Some(n - 1) } else { None };
        let next = if at.index + 1 < n { Some(at.index + 1) } else if sp.closed { Some(0) } else { None };
        (prev.map(|i| sp.anchors[i].p), next.map(|i| sp.anchors[i].p))
    }

    /// Switch an anchor between corner and smooth (Direct select's
    /// double-click): a smooth or symmetric anchor becomes a corner with
    /// its handles pulled in (a sharp point); a corner becomes smooth with
    /// collinear handles (when both were zero, handles a third of the way
    /// toward each neighbor, along the line between the neighbors). Answers
    /// the new mode.
    pub fn toggle_smooth(&mut self, at: AnchorRef) -> Option<AnchorMode> {
        let mode = self.anchor(at)?.mode;
        let to = if mode == AnchorMode::Corner { AnchorMode::Smooth } else { AnchorMode::Corner };
        self.set_mode(at, to);
        Some(to)
    }

    /// Give an anchor a mode, its handles made to agree: corner pulls them
    /// in; smooth makes them collinear, each keeping its length (sensible
    /// handles when both are zero); symmetric also makes them the same
    /// length (their mean).
    pub fn set_mode(&mut self, at: AnchorRef, mode: AnchorMode) {
        let (prev, next) = self.neighbors(at);
        let Some(a) = self.anchor_mut(at) else { return };
        a.mode = mode;
        if mode == AnchorMode::Corner {
            a.h_in = Point::default();
            a.h_out = Point::default();
            return;
        }
        let (li, lo) = (len(a.h_in), len(a.h_out));
        // The direction the curve passes through the anchor (toward `next`).
        let dir = if li > 1e-6 && lo > 1e-6 {
            normalized(sub(a.h_out, a.h_in))
        } else if lo > 1e-6 {
            normalized(a.h_out)
        } else if li > 1e-6 {
            normalized(mul(a.h_in, -1.0))
        } else {
            match (prev, next) {
                (Some(p), Some(n)) => normalized(sub(n, p)),
                (None, Some(n)) => normalized(sub(n, a.p)),
                (Some(p), None) => normalized(sub(a.p, p)),
                (None, None) => None,
            }
        };
        let Some(dir) = dir else { return };
        let li = if li > 1e-6 { li } else { prev.map_or(0.0, |p| dist(p, a.p) / 3.0) };
        let lo = if lo > 1e-6 { lo } else { next.map_or(0.0, |n| dist(n, a.p) / 3.0) };
        let (li, lo) = if mode == AnchorMode::Symmetric {
            let m = if li > 0.0 && lo > 0.0 { (li + lo) / 2.0 } else { li.max(lo) };
            (m, m)
        } else {
            (li, lo)
        };
        a.h_in = mul(dir, -li);
        a.h_out = mul(dir, lo);
    }

    /// Move an anchor (its handles come with it) to `to`.
    pub fn move_anchor(&mut self, at: AnchorRef, to: Point) {
        if let Some(a) = self.anchor_mut(at) {
            a.p = to;
        }
    }

    /// Move anchors by (`dx`, `dy`), each once however often it is named.
    pub fn move_anchors(&mut self, refs: &[AnchorRef], dx: f32, dy: f32) {
        let mut refs = refs.to_vec();
        refs.sort_unstable();
        refs.dedup();
        for r in refs {
            if let Some(a) = self.anchor_mut(r) {
                a.p = Point::new(a.p.x + dx, a.p.y + dy);
            }
        }
    }

    /// Drag a handle's tip to `to` (absolute), the anchor's mode deciding
    /// the other handle: a corner's moves alone; a smooth anchor's other
    /// handle turns to stay opposite, keeping its length; a symmetric
    /// anchor's mirrors it. `break_link` (Alt-drag) makes the anchor a
    /// corner and moves this handle alone.
    pub fn move_handle(&mut self, handle: HandleRef, to: Point, break_link: bool) {
        let Some(a) = self.anchor_mut(handle.anchor) else { return };
        let new = sub(to, a.p);
        if break_link {
            a.mode = AnchorMode::Corner;
        }
        let other = match handle.side {
            HandleSide::In => a.h_out,
            HandleSide::Out => a.h_in,
        };
        let other = match a.mode {
            AnchorMode::Corner => other,
            AnchorMode::Smooth => match normalized(new) {
                Some(d) if len(other) > 1e-6 => mul(d, -len(other)),
                _ => other,
            },
            AnchorMode::Symmetric => mul(new, -1.0),
        };
        match handle.side {
            HandleSide::In => {
                a.h_in = new;
                a.h_out = other;
            }
            HandleSide::Out => {
                a.h_out = new;
                a.h_in = other;
            }
        }
    }

    /// Close subpath `subpath` (the Pen clicking its first anchor).
    pub fn close(&mut self, subpath: usize) {
        if let Some(sp) = self.subpaths.get_mut(subpath) {
            sp.closed = true;
        }
    }
}

// ── Live shapes as paths ────────────────────────────────────────────────────

/// The handle length of a quarter circle of radius 1 made of one cubic.
pub const KAPPA: f32 = 0.552_284_8;

/// A rectangle, its corners rounded by `radius` (clamped to half its
/// shorter side; 0 is square corners). Clockwise from the top left.
pub fn rect(r: &Rect, radius: f32) -> PathGeom {
    let r = r.normalized();
    let rad = radius.max(0.0).min(r.w / 2.0).min(r.h / 2.0);
    if rad <= 1e-4 {
        return PathGeom::polyline(
            &[Point::new(r.x, r.y), Point::new(r.right(), r.y), Point::new(r.right(), r.bottom()), Point::new(r.x, r.bottom())],
            true,
        );
    }
    let k = rad * KAPPA;
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    // Each corner is two anchors: where the straight side ends and the arc
    // begins, and where it ends.
    let a = |x: f32, y: f32, hi: Point, ho: Point| Anchor::new(Point::new(x, y), hi, ho, AnchorMode::Corner);
    let z = Point::default();
    let anchors = vec![
        a(x0 + rad, y0, Point::new(-k, 0.0), z),
        a(x1 - rad, y0, z, Point::new(k, 0.0)),
        a(x1, y0 + rad, Point::new(0.0, -k), z),
        a(x1, y1 - rad, z, Point::new(0.0, k)),
        a(x1 - rad, y1, Point::new(k, 0.0), z),
        a(x0 + rad, y1, z, Point::new(-k, 0.0)),
        a(x0, y1 - rad, Point::new(0.0, k), z),
        a(x0, y0 + rad, z, Point::new(0.0, -k)),
    ];
    PathGeom::from_anchors(anchors, true)
}

/// The ellipse filling `r`: four symmetric anchors, top, right, bottom,
/// left.
pub fn ellipse(r: &Rect) -> PathGeom {
    let r = r.normalized();
    let c = r.center();
    let (rx, ry) = (r.w / 2.0, r.h / 2.0);
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    let s = AnchorMode::Symmetric;
    PathGeom::from_anchors(
        vec![
            Anchor::new(Point::new(c.x, r.y), Point::new(-kx, 0.0), Point::new(kx, 0.0), s),
            Anchor::new(Point::new(r.right(), c.y), Point::new(0.0, -ky), Point::new(0.0, ky), s),
            Anchor::new(Point::new(c.x, r.bottom()), Point::new(kx, 0.0), Point::new(-kx, 0.0), s),
            Anchor::new(Point::new(r.x, c.y), Point::new(0.0, ky), Point::new(0.0, -ky), s),
        ],
        true,
    )
}

/// The diamond in `r`: its four side midpoints.
pub fn diamond(r: &Rect) -> PathGeom {
    let r = r.normalized();
    let c = r.center();
    PathGeom::polyline(&[Point::new(c.x, r.y), Point::new(r.right(), c.y), Point::new(c.x, r.bottom()), Point::new(r.x, c.y)], true)
}

/// Points around the unit circle starting at the top, clockwise, then
/// stretched so their extent fills `r` exactly.
fn fitted(r: &Rect, unit: Vec<Point>) -> Vec<Point> {
    let r = r.normalized();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in &unit {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    let (w, h) = ((x1 - x0).max(1e-6), (y1 - y0).max(1e-6));
    unit.into_iter().map(|p| Point::new(r.x + (p.x - x0) / w * r.w, r.y + (p.y - y0) / h * r.h)).collect()
}

/// A regular polygon of `sides` (at least 3) with a vertex at the top,
/// stretched to fill `r` (so its box is the element's box).
pub fn polygon(r: &Rect, sides: u32) -> PathGeom {
    let n = sides.max(3);
    let unit = (0..n)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 / n as f32 * std::f32::consts::TAU;
            Point::new(a.cos(), a.sin())
        })
        .collect();
    PathGeom::polyline(&fitted(r, unit), true)
}

/// A star of `points` tips (at least 3) with one at the top, its inner
/// corners at `inner` times the outer radius, stretched to fill `r`.
pub fn star(r: &Rect, points: u32, inner: f32) -> PathGeom {
    let n = points.max(3);
    let inner = inner.clamp(0.0, 1.0);
    let unit = (0..2 * n)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 / (2 * n) as f32 * std::f32::consts::TAU;
            let k = if i % 2 == 0 { 1.0 } else { inner };
            Point::new(a.cos() * k, a.sin() * k)
        })
        .collect();
    PathGeom::polyline(&fitted(r, unit), true)
}

/// A live shape's outline as a path, with its style's parameters
/// (rectangle radius, polygon sides, star points and inner radius).
pub fn shape(kind: &Kind, r: &Rect, style: &ResolvedStyle) -> Option<PathGeom> {
    Some(match kind {
        Kind::Rect => rect(r, style.radius),
        Kind::Ellipse => ellipse(r),
        Kind::Diamond => diamond(r),
        Kind::Polygon => polygon(r, style.sides),
        Kind::Star => star(r, style.points, style.inner),
        _ => return None,
    })
}

/// Pen strokes are fitted this closely when turned into a path.
pub const STROKE_TO_PATH_TOLERANCE: f32 = 0.5;

/// An element's geometry as a path, in its own element space: a live shape
/// (rectangle with its radius, ellipse, diamond, polygon, star), a line
/// with no arrow heads, a pen stroke (fitted within
/// [`STROKE_TO_PATH_TOLERANCE`]; closed when it has a fill, as it is drawn),
/// a path itself. `None` for everything else (an arrow, text, a sticky
/// note, a picture...).
pub fn to_path(kind: &Kind, geom: &Geom, style: &Style, points: &[[f32; 3]]) -> Option<PathGeom> {
    let s = style.resolved(kind);
    match (kind, geom) {
        (Kind::Path, Geom::Path(p)) => Some(p.clone()),
        (k, Geom::Rect(r)) if k.is_shape() => shape(k, r, &s),
        (Kind::Line, Geom::Segment { a, b }) if s.arrow_heads == ArrowHeads::None => Some(PathGeom::polyline(&[*a, *b], false)),
        (Kind::Pen, _) => {
            let mut p = fit_stroke(points, STROKE_TO_PATH_TOLERANCE);
            if p.is_empty() {
                return None;
            }
            if s.fill.is_some_and(|f| f.a > 0) {
                for sp in &mut p.subpaths {
                    sp.closed = true;
                }
            }
            Some(p)
        }
        _ => None,
    }
}

// ── Fitting (the Pencil) ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
struct V(f64, f64);

impl V {
    fn of(p: Point) -> V {
        V(p.x as f64, p.y as f64)
    }
    fn pt(self) -> Point {
        Point::new(self.0 as f32, self.1 as f32)
    }
    fn add(self, o: V) -> V {
        V(self.0 + o.0, self.1 + o.1)
    }
    fn sub(self, o: V) -> V {
        V(self.0 - o.0, self.1 - o.1)
    }
    fn mul(self, k: f64) -> V {
        V(self.0 * k, self.1 * k)
    }
    fn dot(self, o: V) -> f64 {
        self.0 * o.0 + self.1 * o.1
    }
    fn len(self) -> f64 {
        self.dot(self).sqrt()
    }
    fn unit(self) -> V {
        let l = self.len();
        if l > 1e-12 {
            self.mul(1.0 / l)
        } else {
            V(0.0, 0.0)
        }
    }
}

type Bez = [V; 4];

fn bez_at(b: &Bez, t: f64) -> V {
    let mt = 1.0 - t;
    b[0].mul(mt * mt * mt).add(b[1].mul(3.0 * mt * mt * t)).add(b[2].mul(3.0 * mt * t * t)).add(b[3].mul(t * t * t))
}

fn bez_d1(b: &Bez, t: f64) -> V {
    let mt = 1.0 - t;
    b[1].sub(b[0]).mul(3.0 * mt * mt).add(b[2].sub(b[1]).mul(6.0 * mt * t)).add(b[3].sub(b[2]).mul(3.0 * t * t))
}

fn bez_d2(b: &Bez, t: f64) -> V {
    let mt = 1.0 - t;
    b[2].sub(b[1].mul(2.0)).add(b[0]).mul(6.0 * mt).add(b[3].sub(b[2].mul(2.0)).add(b[1]).mul(6.0 * t))
}

/// Freehand points fitted to cubic Béziers (Schneider, "An Algorithm for
/// Automatically Fitting Digitized Curves", Graphics Gems 1990): every
/// point is within `tolerance` board units of the curve. Sharp turns (more
/// than about 60 degrees) become corner anchors; the joins between fitted
/// curves are smooth. One subpath, open (a loop drawn closed stays open:
/// its ends meet but are not joined). Fewer than two distinct points give a
/// dot: two anchors a hair apart.
pub fn fit(points: &[Point], tolerance: f32) -> PathGeom {
    let tol = (tolerance as f64).max(1e-3);
    let mut d: Vec<V> = Vec::with_capacity(points.len());
    for p in points {
        if !(p.x.is_finite() && p.y.is_finite()) {
            continue;
        }
        let v = V::of(*p);
        if d.last().is_none_or(|q: &V| v.sub(*q).len() > 1e-6) {
            d.push(v);
        }
    }
    match d.len() {
        0 => return PathGeom::default(),
        1 => {
            let p = d[0].pt();
            return PathGeom::polyline(&[p, Point::new(p.x + 0.01, p.y)], false);
        }
        _ => {}
    }
    let corners = find_corners(&d, tol);
    // Split at corners and fit each run.
    let mut cuts = vec![0];
    cuts.extend(corners.iter().copied());
    cuts.push(d.len() - 1);
    let mut beziers: Vec<(Bez, bool)> = Vec::new(); // (curve, starts at a corner)
    for w in cuts.windows(2) {
        let (first, last) = (w[0], w[1]);
        if last <= first {
            continue;
        }
        let t1 = left_tangent(&d, first);
        let t2 = right_tangent(&d, last);
        let mut run = Vec::new();
        fit_cubic(&d, first, last, t1, t2, tol * tol, &mut run, 0);
        for (k, b) in run.into_iter().enumerate() {
            beziers.push((b, k == 0));
        }
    }
    // Beziers to anchors.
    let mut anchors: Vec<Anchor> = Vec::with_capacity(beziers.len() + 1);
    for (k, (b, starts_run)) in beziers.iter().enumerate() {
        let p0 = b[0].pt();
        if k == 0 {
            anchors.push(Anchor { p: p0, h_in: Point::default(), h_out: sub(b[1].pt(), p0), mode: AnchorMode::Corner });
        } else {
            let a = anchors.last_mut().unwrap();
            a.h_out = sub(b[1].pt(), a.p);
            a.mode = if *starts_run {
                AnchorMode::Corner
            } else if !is_zero(a.h_in) && !is_zero(a.h_out) {
                AnchorMode::Smooth
            } else {
                AnchorMode::Corner
            };
        }
        let p3 = b[3].pt();
        anchors.push(Anchor { p: p3, h_in: sub(b[2].pt(), p3), h_out: Point::default(), mode: AnchorMode::Corner });
    }
    // A straight piece's handles lie on its chord: make it a plain line.
    for i in 0..anchors.len().saturating_sub(1) {
        let (a, b) = (anchors[i], anchors[i + 1]);
        let chord = sub(b.p, a.p);
        let lc = len(chord);
        if lc < 1e-6 {
            continue;
        }
        let off = |h: Point, from: Point| {
            let v = sub(add(from, h), a.p);
            (v.x * chord.y - v.y * chord.x).abs() / lc
        };
        if off(a.h_out, a.p) < 1e-3 && off(b.h_in, b.p) < 1e-3 && a.mode == AnchorMode::Corner && b.mode == AnchorMode::Corner {
            anchors[i].h_out = Point::default();
            anchors[i + 1].h_in = Point::default();
        }
    }
    PathGeom::from_anchors(anchors, false)
}

/// [`fit`] over a stroke's (x, y, pressure) points; pressure is ignored.
pub fn fit_stroke(points: &[[f32; 3]], tolerance: f32) -> PathGeom {
    let pts: Vec<Point> = points.iter().map(|p| Point::new(p[0], p[1])).collect();
    fit(&pts, tolerance)
}

/// The indices of sharp turns: the turn between the directions to points
/// about `2 * tol` away on either side passes 60 degrees, the sharpest in
/// its neighborhood.
fn find_corners(d: &[V], tol: f64) -> Vec<usize> {
    let n = d.len();
    if n < 3 {
        return Vec::new();
    }
    let reach = (tol * 2.0).max(1e-3);
    // Arc length at each point.
    let mut s = vec![0.0f64; n];
    for i in 1..n {
        s[i] = s[i - 1] + d[i].sub(d[i - 1]).len();
    }
    let mut angle = vec![0.0f64; n];
    for i in 1..n - 1 {
        let mut j = i - 1;
        while j > 0 && s[i] - s[j] < reach {
            j -= 1;
        }
        let mut k = i + 1;
        while k < n - 1 && s[k] - s[i] < reach {
            k += 1;
        }
        let (a, b) = (d[i].sub(d[j]).unit(), d[k].sub(d[i]).unit());
        if a.len() == 0.0 || b.len() == 0.0 {
            continue;
        }
        angle[i] = a.dot(b).clamp(-1.0, 1.0).acos();
    }
    let limit = 60f64.to_radians();
    let mut out = Vec::new();
    for i in 1..n - 1 {
        if angle[i] < limit {
            continue;
        }
        // The sharpest within `reach` along the stroke (the earliest of equals).
        let sharpest = (1..n - 1).filter(|&j| (s[j] - s[i]).abs() < reach).all(|j| angle[j] < angle[i] || (angle[j] == angle[i] && j >= i));
        if sharpest && out.last().is_none_or(|&l: &usize| s[i] - s[l] >= reach * 0.5) {
            out.push(i);
        }
    }
    out
}

fn left_tangent(d: &[V], first: usize) -> V {
    d[first + 1].sub(d[first]).unit()
}

fn right_tangent(d: &[V], last: usize) -> V {
    d[last - 1].sub(d[last]).unit()
}

fn center_tangent(d: &[V], center: usize) -> V {
    let v1 = d[center - 1].sub(d[center]);
    let v2 = d[center].sub(d[center + 1]);
    let t = v1.add(v2).mul(0.5).unit();
    if t.len() == 0.0 {
        d[center - 1].sub(d[center + 1]).unit()
    } else {
        t
    }
}

fn fit_cubic(d: &[V], first: usize, last: usize, t1: V, t2: V, err2: f64, out: &mut Vec<Bez>, depth: u32) {
    let n = last - first + 1;
    if n == 2 || depth > 64 {
        // Exactly the chord: a line (its handles a third of the way along).
        let third = d[last].sub(d[first]).mul(1.0 / 3.0);
        out.push([d[first], d[first].add(third), d[last].sub(third), d[last]]);
        return;
    }
    let mut u = chord_params(d, first, last);
    let mut bez = generate(d, first, last, &u, t1, t2);
    let (mut max, mut split) = max_error(d, first, last, &bez, &u);
    if max < err2 {
        out.push(bez);
        return;
    }
    if max < err2 * 16.0 {
        for _ in 0..8 {
            u = reparameterize(d, first, last, &u, &bez);
            bez = generate(d, first, last, &u, t1, t2);
            let (m, s) = max_error(d, first, last, &bez, &u);
            max = m;
            split = s;
            if max < err2 {
                out.push(bez);
                return;
            }
        }
    }
    let split = split.clamp(first + 1, last - 1);
    let tc = center_tangent(d, split);
    fit_cubic(d, first, split, t1, tc, err2, out, depth + 1);
    fit_cubic(d, split, last, tc.mul(-1.0), t2, err2, out, depth + 1);
}

fn chord_params(d: &[V], first: usize, last: usize) -> Vec<f64> {
    let mut u = vec![0.0; last - first + 1];
    for i in first + 1..=last {
        u[i - first] = u[i - first - 1] + d[i].sub(d[i - 1]).len();
    }
    let total = u[last - first].max(1e-12);
    for v in &mut u {
        *v /= total;
    }
    u
}

fn generate(d: &[V], first: usize, last: usize, u: &[f64], t1: V, t2: V) -> Bez {
    let (p0, p3) = (d[first], d[last]);
    let (mut c00, mut c01, mut c11, mut x0, mut x1) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (k, &t) in u.iter().enumerate() {
        let mt = 1.0 - t;
        let (b0, b1, b2, b3) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
        let a0 = t1.mul(b1);
        let a1 = t2.mul(b2);
        c00 += a0.dot(a0);
        c01 += a0.dot(a1);
        c11 += a1.dot(a1);
        let tmp = d[first + k].sub(p0.mul(b0 + b1).add(p3.mul(b2 + b3)));
        x0 += a0.dot(tmp);
        x1 += a1.dot(tmp);
    }
    let det = c00 * c11 - c01 * c01;
    let (mut al, mut ar) = if det.abs() > 1e-12 { ((x0 * c11 - x1 * c01) / det, (c00 * x1 - c01 * x0) / det) } else { (0.0, 0.0) };
    let seg = p3.sub(p0).len();
    let eps = 1e-6 * seg;
    if al < eps || ar < eps {
        al = seg / 3.0;
        ar = seg / 3.0;
    }
    [p0, p0.add(t1.mul(al)), p3.add(t2.mul(ar)), p3]
}

fn max_error(d: &[V], first: usize, last: usize, bez: &Bez, u: &[f64]) -> (f64, usize) {
    let mut max = 0.0;
    let mut split = (first + last) / 2;
    for i in first + 1..last {
        let e = bez_at(bez, u[i - first]).sub(d[i]);
        let e2 = e.dot(e);
        if e2 >= max {
            max = e2;
            split = i;
        }
    }
    (max, split)
}

fn reparameterize(d: &[V], first: usize, last: usize, u: &[f64], bez: &Bez) -> Vec<f64> {
    (first..=last)
        .map(|i| {
            let t = u[i - first];
            let q = bez_at(bez, t);
            let q1 = bez_d1(bez, t);
            let q2 = bez_d2(bez, t);
            let diff = q.sub(d[i]);
            let num = diff.dot(q1);
            let den = q1.dot(q1) + diff.dot(q2);
            if den.abs() < 1e-12 {
                t
            } else {
                (t - num / den).clamp(0.0, 1.0)
            }
        })
        .collect()
}

// ── SVG path data ───────────────────────────────────────────────────────────

/// A number for path data: at most three decimals, no trailing zeros.
fn num(v: f32) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".into()
    } else {
        s.to_string()
    }
}

/// SVG path data for a path: absolute `M`, `L` (straight segments), `C`
/// and `Z`. Numbers keep three decimals. [`parse_d`] reads it back to the
/// same anchors (modes inferred from the handles).
pub fn to_d(path: &PathGeom) -> String {
    let mut d = String::new();
    let seg = |d: &mut String, a: &Anchor, b: &Anchor| {
        if is_zero(a.h_out) && is_zero(b.h_in) {
            let _ = write!(d, "L{} {}", num(b.p.x), num(b.p.y));
        } else {
            let (c1, c2) = (a.out_point(), b.in_point());
            let _ = write!(d, "C{} {} {} {} {} {}", num(c1.x), num(c1.y), num(c2.x), num(c2.y), num(b.p.x), num(b.p.y));
        }
    };
    for sp in &path.subpaths {
        let Some(first) = sp.anchors.first() else { continue };
        let _ = write!(d, "M{} {}", num(first.p.x), num(first.p.y));
        for w in sp.anchors.windows(2) {
            seg(&mut d, &w[0], &w[1]);
        }
        if sp.closed {
            let last = sp.anchors.last().unwrap();
            if sp.anchors.len() >= 2 && !(is_zero(last.h_out) && is_zero(first.h_in)) {
                seg(&mut d, last, first);
            }
            d.push('Z');
        }
    }
    d
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

impl Lexer<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',') {
            self.i += 1;
        }
    }

    fn err(&self, message: &str) -> PathError {
        PathError::Syntax { at: self.i, message: message.to_string() }
    }

    fn command(&mut self) -> Option<u8> {
        self.skip();
        let c = *self.s.get(self.i)?;
        c.is_ascii_alphabetic().then(|| {
            self.i += 1;
            c
        })
    }

    fn at_number(&mut self) -> bool {
        self.skip();
        matches!(self.s.get(self.i), Some(c) if c.is_ascii_digit() || *c == b'-' || *c == b'+' || *c == b'.')
    }

    fn number(&mut self) -> Result<f32, PathError> {
        self.skip();
        let start = self.i;
        let s = self.s;
        let mut i = self.i;
        if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
            i += 1;
        }
        let mut digits = 0;
        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
        if i < s.len() && s[i] == b'.' {
            i += 1;
            while i < s.len() && s[i].is_ascii_digit() {
                i += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            return Err(self.err("a number was expected"));
        }
        if i < s.len() && (s[i] == b'e' || s[i] == b'E') {
            let mut j = i + 1;
            if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
                j += 1;
            }
            if j < s.len() && s[j].is_ascii_digit() {
                while j < s.len() && s[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
            }
        }
        let text = std::str::from_utf8(&s[start..i]).map_err(|_| self.err("a number was expected"))?;
        let v: f32 = text.parse().map_err(|_| self.err("a number was expected"))?;
        if !v.is_finite() {
            return Err(self.err("the number is too large"));
        }
        self.i = i;
        Ok(v)
    }

    /// An arc flag: one character, 0 or 1 (flags may run together).
    fn flag(&mut self) -> Result<bool, PathError> {
        self.skip();
        match self.s.get(self.i) {
            Some(b'0') => {
                self.i += 1;
                Ok(false)
            }
            Some(b'1') => {
                self.i += 1;
                Ok(true)
            }
            _ => Err(self.err("an arc flag (0 or 1) was expected")),
        }
    }

    fn point(&mut self) -> Result<Point, PathError> {
        let x = self.number()?;
        let y = self.number()?;
        Ok(Point::new(x, y))
    }
}

#[derive(Default)]
struct Builder {
    out: Vec<Subpath>,
    cur: Option<Subpath>,
    pos: Point,
    start: Point,
}

impl Builder {
    fn move_to(&mut self, p: Point) {
        self.finish_open();
        self.cur = Some(Subpath::new(vec![Anchor::corner(p)], false));
        self.pos = p;
        self.start = p;
    }

    fn current(&mut self) -> &mut Subpath {
        if self.cur.is_none() {
            // A drawing command after Z starts a new subpath where the last began.
            self.cur = Some(Subpath::new(vec![Anchor::corner(self.start)], false));
            self.pos = self.start;
        }
        self.cur.as_mut().unwrap()
    }

    fn line_to(&mut self, p: Point) {
        self.current().anchors.push(Anchor::corner(p));
        self.pos = p;
    }

    fn cubic_to(&mut self, c1: Point, c2: Point, p: Point) {
        let pos = self.pos;
        let sp = self.current();
        if let Some(last) = sp.anchors.last_mut() {
            last.h_out = sub(c1, pos);
        }
        sp.anchors.push(Anchor { p, h_in: sub(c2, p), h_out: Point::default(), mode: AnchorMode::Corner });
        self.pos = p;
    }

    fn close(&mut self) {
        if let Some(mut sp) = self.cur.take() {
            let n = sp.anchors.len();
            if n >= 2 && dist(sp.anchors[n - 1].p, sp.anchors[0].p) < 1e-4 {
                let last = sp.anchors.pop().unwrap();
                sp.anchors[0].h_in = last.h_in;
            }
            sp.closed = true;
            self.out.push(sp);
        }
        self.pos = self.start;
    }

    fn finish_open(&mut self) {
        if let Some(sp) = self.cur.take() {
            self.out.push(sp);
        }
    }
}

/// The cubics (control 1, control 2, end) of an SVG elliptical arc from
/// `p0` to `p`, at most a quarter turn each (SVG 1.1 appendix F.6).
fn arc_cubics(p0: Point, rx: f32, ry: f32, phi_deg: f32, large: bool, sweep: bool, p: Point) -> Option<Vec<[Point; 3]>> {
    if dist(p0, p) < 1e-9 {
        return Some(Vec::new());
    }
    let (mut rx, mut ry) = ((rx as f64).abs(), (ry as f64).abs());
    if rx < 1e-12 || ry < 1e-12 {
        return None; // a straight line
    }
    let phi = (phi_deg as f64).to_radians();
    let (sin, cos) = phi.sin_cos();
    let (x0, y0, x1, y1) = (p0.x as f64, p0.y as f64, p.x as f64, p.y as f64);
    let dx2 = (x0 - x1) / 2.0;
    let dy2 = (y0 - y1) / 2.0;
    let x1p = cos * dx2 + sin * dy2;
    let y1p = -sin * dx2 + cos * dy2;
    let lambda = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry);
    if lambda > 1.0 {
        let k = lambda.sqrt();
        rx *= k;
        ry *= k;
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = if den > 0.0 { (num / den).max(0.0).sqrt() } else { 0.0 };
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * y1p / ry;
    let cyp = -coef * ry * x1p / rx;
    let cx = cos * cxp - sin * cyp + (x0 + x1) / 2.0;
    let cy = sin * cxp + cos * cyp + (y0 + y1) / 2.0;
    let angle = |ux: f64, uy: f64, vx: f64, vy: f64| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let (ux, uy) = ((x1p - cxp) / rx, (y1p - cyp) / ry);
    let (vx, vy) = ((-x1p - cxp) / rx, (-y1p - cyp) / ry);
    let theta1 = angle(1.0, 0.0, ux, uy);
    let mut dtheta = angle(ux, uy, vx, vy);
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f64::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f64::consts::TAU;
    }
    let segs = ((dtheta.abs() / std::f64::consts::FRAC_PI_2) - 1e-9).ceil().max(1.0) as usize;
    let delta = dtheta / segs as f64;
    let k = 4.0 / 3.0 * (delta / 4.0).tan();
    let map = |ux: f64, uy: f64| Point::new((cx + rx * cos * ux - ry * sin * uy) as f32, (cy + rx * sin * ux + ry * cos * uy) as f32);
    let mut out = Vec::with_capacity(segs);
    for i in 0..segs {
        let a = theta1 + i as f64 * delta;
        let b = a + delta;
        let (sa, ca) = a.sin_cos();
        let (sb, cb) = b.sin_cos();
        let c1 = map(ca - k * sa, sa + k * ca);
        let c2 = map(cb + k * sb, sb - k * cb);
        let end = if i + 1 == segs { p } else { map(cb, sb) };
        out.push([c1, c2, end]);
    }
    Some(out)
}

/// A path from SVG path data: `M L H V C S Q T A Z`, absolute (upper case)
/// and relative (lower case), implicit repeats, numbers run together as
/// SVG allows (`1.5.5`, `-1-2`, arc flags without separators). Quadratics
/// and arcs become cubics. A closing segment that returns to the start
/// point merges into the first anchor. Anchor modes are inferred from the
/// handles ([`infer_mode`]). Subpaths of a single point are dropped; data
/// that draws nothing is [`PathError::Empty`].
pub fn parse_d(d: &str) -> Result<PathGeom, PathError> {
    let mut lx = Lexer { s: d.as_bytes(), i: 0 };
    let mut b = Builder::default();
    // The previous command's second control point (C/S) or control (Q/T),
    // for the reflections of S and T.
    let mut last_c2: Option<Point> = None;
    let mut last_q: Option<Point> = None;
    let mut cmd: Option<u8> = None;
    let mut first = true;
    loop {
        let c = match lx.command() {
            Some(c) => c,
            None => {
                lx.skip();
                if lx.i >= lx.s.len() {
                    break;
                }
                // Implicit repeat of the last command (after M, it is L).
                match cmd {
                    Some(b'M') => b'L',
                    Some(b'm') => b'l',
                    Some(b'Z' | b'z') | None => return Err(lx.err("a command letter was expected")),
                    Some(c) => {
                        if !lx.at_number() {
                            return Err(lx.err("a number was expected"));
                        }
                        c
                    }
                }
            }
        };
        if first && !matches!(c, b'M' | b'm') {
            return Err(lx.err("path data starts with M"));
        }
        first = false;
        let rel = c.is_ascii_lowercase();
        let base = if rel { b.pos } else { Point::default() };
        let at = |p: Point| add(base, p);
        let mut c2: Option<Point> = None;
        let mut q: Option<Point> = None;
        match c.to_ascii_uppercase() {
            b'M' => {
                let p = at(lx.point()?);
                b.move_to(p);
            }
            b'L' => {
                let p = at(lx.point()?);
                b.line_to(p);
            }
            b'H' => {
                let x = lx.number()?;
                let p = Point::new(if rel { b.pos.x + x } else { x }, b.pos.y);
                b.line_to(p);
            }
            b'V' => {
                let y = lx.number()?;
                let p = Point::new(b.pos.x, if rel { b.pos.y + y } else { y });
                b.line_to(p);
            }
            b'C' => {
                let c1 = at(lx.point()?);
                let cc2 = at(lx.point()?);
                let p = at(lx.point()?);
                b.cubic_to(c1, cc2, p);
                c2 = Some(cc2);
            }
            b'S' => {
                let cc2 = at(lx.point()?);
                let p = at(lx.point()?);
                let c1 = match last_c2 {
                    Some(prev) => sub(mul(b.pos, 2.0), prev),
                    None => b.pos,
                };
                b.cubic_to(c1, cc2, p);
                c2 = Some(cc2);
            }
            b'Q' => {
                let qq = at(lx.point()?);
                let p = at(lx.point()?);
                let p0 = b.pos;
                b.cubic_to(lerp(p0, qq, 2.0 / 3.0), lerp(p, qq, 2.0 / 3.0), p);
                q = Some(qq);
            }
            b'T' => {
                let p = at(lx.point()?);
                let p0 = b.pos;
                let qq = match last_q {
                    Some(prev) => sub(mul(p0, 2.0), prev),
                    None => p0,
                };
                b.cubic_to(lerp(p0, qq, 2.0 / 3.0), lerp(p, qq, 2.0 / 3.0), p);
                q = Some(qq);
            }
            b'A' => {
                let rx = lx.number()?;
                let ry = lx.number()?;
                let rot = lx.number()?;
                let large = lx.flag()?;
                let sweep = lx.flag()?;
                let p = at(lx.point()?);
                match arc_cubics(b.pos, rx, ry, rot, large, sweep, p) {
                    Some(cubics) => {
                        for [c1, cc2, end] in cubics {
                            b.cubic_to(c1, cc2, end);
                        }
                    }
                    None => b.line_to(p),
                }
            }
            b'Z' => b.close(),
            _ => return Err(PathError::Syntax { at: lx.i.saturating_sub(1), message: format!("{} is not a path command", c as char) }),
        }
        last_c2 = c2;
        last_q = q;
        cmd = Some(c);
    }
    b.finish_open();
    let mut subpaths: Vec<Subpath> = b.out.into_iter().filter(|sp| sp.anchors.len() >= 2).collect();
    for sp in &mut subpaths {
        for a in &mut sp.anchors {
            a.mode = infer_mode(a.h_in, a.h_out);
        }
    }
    if subpaths.is_empty() {
        return Err(PathError::Empty);
    }
    Ok(PathGeom { subpaths, rule: PathRule::NonZero })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close_to(a: Point, b: Point, eps: f32) -> bool {
        dist(a, b) <= eps
    }

    #[test]
    fn splitting_a_segment_keeps_its_shape() {
        let mut p = parse_d("M0 0 C30 -40 70 -40 100 0").unwrap();
        let before: Vec<Point> = (0..=50).map(|i| cubic_at(&p.subpaths[0].segment(0).unwrap(), i as f32 / 50.0)).collect();
        let at = p.insert_anchor(0, 0, 0.3).unwrap();
        assert_eq!(at, AnchorRef::new(0, 1));
        assert_eq!(p.subpaths[0].anchors.len(), 3);
        for q in before {
            assert!(p.distance_to(q) < 0.01, "{q:?} is still on the curve");
        }
        assert_eq!(p.anchor(at).unwrap().mode, AnchorMode::Smooth);
    }

    #[test]
    fn a_closed_path_round_trips_and_merges_its_closing_point() {
        let p = ellipse(&Rect::new(0.0, 0.0, 100.0, 50.0));
        let d = to_d(&p);
        let back = parse_d(&d).unwrap();
        assert_eq!(back.subpaths.len(), 1);
        assert!(back.subpaths[0].closed);
        assert_eq!(back.subpaths[0].anchors.len(), 4);
        for (a, b) in p.subpaths[0].anchors.iter().zip(&back.subpaths[0].anchors) {
            assert!(close_to(a.p, b.p, 1e-3) && close_to(a.h_in, b.h_in, 1e-3) && close_to(a.h_out, b.h_out, 1e-3));
            assert_eq!(b.mode, AnchorMode::Symmetric);
        }
    }
}
