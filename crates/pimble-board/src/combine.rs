//! Combining paths (docs/DRAWING_CONTRACT.md section 2, "Combining"):
//! union, subtract, intersect and exclude over the filled areas of two or
//! more paths, on `linesweeper` (a robust sweep line over Bézier curves:
//! coincident edges, shared corners, self-intersections and either fill rule
//! come out right; see the contract's "Spike: combining").
//!
//! Pure: the paths go in already in one space (the board's), bottom to top,
//! and one path comes out in that space. [`crate::BoardTxn::combine`] is the
//! edit that reads the elements, maps them through their transforms and
//! writes the result.

use kurbo::BezPath;
use linesweeper::topology::{BinaryWindingNumber, Topology};

use crate::model::Point;
use crate::path::{parse_d, PathGeom, PathRule};

/// What a combination keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CombineOp {
    /// Everything any operand covers.
    Union,
    /// The bottom operand minus everything above it (Illustrator's "Minus
    /// Front").
    Subtract,
    /// What every operand covers.
    Intersect,
    /// What an odd number of operands cover (overlaps of two cut out).
    Exclude,
}

impl CombineOp {
    pub const ALL: [CombineOp; 4] = [CombineOp::Union, CombineOp::Subtract, CombineOp::Intersect, CombineOp::Exclude];

    pub fn as_str(&self) -> &'static str {
        match self {
            CombineOp::Union => "union",
            CombineOp::Subtract => "subtract",
            CombineOp::Intersect => "intersect",
            CombineOp::Exclude => "exclude",
        }
    }

    pub fn parse(s: &str) -> Option<CombineOp> {
        CombineOp::ALL.iter().copied().find(|o| o.as_str() == s)
    }

    /// The name a person reads ("Union").
    pub fn label(&self) -> &'static str {
        match self {
            CombineOp::Union => "Union",
            CombineOp::Subtract => "Subtract",
            CombineOp::Intersect => "Intersect",
            CombineOp::Exclude => "Exclude",
        }
    }
}

/// A path as kurbo's, every subpath closed (an open one fills as if closed,
/// as SVG and the renderer fill it).
pub(crate) fn to_kurbo(p: &PathGeom) -> BezPath {
    let mut out = BezPath::new();
    let pt = |p: Point| kurbo::Point::new(p.x as f64, p.y as f64);
    for sp in &p.subpaths {
        if sp.anchors.len() < 2 {
            continue;
        }
        out.move_to(pt(sp.anchors[0].p));
        for i in 0..sp.segment_count() {
            let Some(c) = sp.segment(i) else { continue };
            if sp.is_line(i) {
                out.line_to(pt(c[3]));
            } else {
                out.curve_to(pt(c[1]), pt(c[2]), pt(c[3]));
            }
        }
        out.close_path();
    }
    out
}

/// kurbo's path as a [`PathGeom`] (through SVG path data, which merges a
/// closing point into the first anchor and infers anchor modes).
pub(crate) fn from_kurbo(b: &BezPath, rule: PathRule) -> Option<PathGeom> {
    let mut p = parse_d(&b.to_svg()).ok()?;
    p.rule = rule;
    Some(p)
}

/// One step: `a` op `b`, each filled by its own rule.
fn step(a: &BezPath, rule_a: PathRule, b: &BezPath, rule_b: PathRule, op: CombineOp) -> Option<BezPath> {
    let bbox = kurbo::Shape::bounding_box(a).union(kurbo::Shape::bounding_box(b));
    let m = [bbox.min_x(), bbox.min_y(), bbox.max_x(), bbox.max_y()].iter().fold(0.0f64, |m, v| m.max(v.abs()));
    if !m.is_finite() {
        return None;
    }
    // As linesweeper's own `binary_op` chooses it.
    let eps = (m * f64::EPSILON * 64.0).max(1e-6);
    let top = Topology::<BinaryWindingNumber>::from_paths_binary(a, b, eps).ok()?;
    let inside = |rule: PathRule, w: i32| match rule {
        PathRule::NonZero => w != 0,
        PathRule::EvenOdd => w % 2 != 0,
    };
    let contours = top.contours(|w: &BinaryWindingNumber| {
        let (ia, ib) = (inside(rule_a, w.shape_a), inside(rule_b, w.shape_b));
        match op {
            CombineOp::Union => ia || ib,
            CombineOp::Subtract => ia && !ib,
            CombineOp::Intersect => ia && ib,
            CombineOp::Exclude => ia != ib,
        }
    });
    let mut out = BezPath::new();
    for c in contours.contours() {
        out.extend(c.path.iter());
    }
    Some(out)
}

/// The paths `operands` (bottom to top, all in one space) combined by `op`:
/// one path, nonzero (its contours wind outer one way and holes the other).
/// [`CombineOp::Subtract`] takes the bottom one minus all the others.
/// `None` when nothing is left (shapes that do not overlap, intersected),
/// or fewer than two operands were given.
pub fn combine(operands: &[PathGeom], op: CombineOp) -> Option<PathGeom> {
    if operands.len() < 2 {
        return None;
    }
    let mut acc = to_kurbo(&operands[0]);
    let mut acc_rule = operands[0].rule;
    for next in &operands[1..] {
        acc = step(&acc, acc_rule, &to_kurbo(next), next.rule, op)?;
        acc_rule = PathRule::NonZero;
    }
    from_kurbo(&acc, PathRule::NonZero).filter(|p| !p.is_empty())
}

/// The filled area of a path (by its rule), in square board units: what
/// tests measure combinations by.
pub fn area(p: &PathGeom) -> f32 {
    // Union with nothing resolves the rule into simple contours, whose
    // signed areas then add up (holes wind the other way).
    let a = to_kurbo(p);
    let Some(simple) = step(&a, p.rule, &BezPath::new(), PathRule::NonZero, CombineOp::Union) else { return 0.0 };
    (kurbo::Shape::area(&simple) as f32).abs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rect;
    use crate::path;

    fn circle(cx: f32, cy: f32, r: f32) -> PathGeom {
        path::ellipse(&Rect::new(cx - r, cy - r, 2.0 * r, 2.0 * r))
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> PathGeom {
        path::rect(&Rect::new(x, y, w, h), 0.0)
    }

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    // The spike's cases (docs/DRAWING_CONTRACT.md, "Spike: combining").

    #[test]
    fn two_overlapping_circles_all_four_ways() {
        let (a, b) = (circle(0.0, 0.0, 50.0), circle(50.0, 0.0, 50.0));
        let lens = 2.0 * 2500.0 * 0.5f32.acos() - 25.0 * 7500f32.sqrt();
        let disc = std::f32::consts::PI * 2500.0;
        // The four-cubic circle is within 0.03% of a true one.
        let tol = 0.004 * disc;
        let u = combine(&[a.clone(), b.clone()], CombineOp::Union).unwrap();
        assert!(close(area(&u), 2.0 * disc - lens, tol), "union {}", area(&u));
        assert_eq!(u.subpaths.len(), 1);
        let s = combine(&[a.clone(), b.clone()], CombineOp::Subtract).unwrap();
        assert!(close(area(&s), disc - lens, tol), "subtract {}", area(&s));
        let i = combine(&[a.clone(), b.clone()], CombineOp::Intersect).unwrap();
        assert!(close(area(&i), lens, tol), "intersect {}", area(&i));
        let x = combine(&[a, b], CombineOp::Exclude).unwrap();
        assert!(close(area(&x), 2.0 * (disc - lens), tol), "exclude {}", area(&x));
        assert_eq!(x.subpaths.len(), 2);
        // The result is curves, not a polygon.
        assert!(u.subpaths[0].anchors.len() < 12, "{} anchors", u.subpaths[0].anchors.len());
        assert!((0..u.subpaths[0].segment_count()).any(|i| !u.subpaths[0].is_line(i)));
    }

    #[test]
    fn rectangles_sharing_an_edge() {
        let (a, b) = (rect(0.0, 0.0, 100.0, 100.0), rect(100.0, 0.0, 100.0, 100.0));
        let u = combine(&[a.clone(), b.clone()], CombineOp::Union).unwrap();
        assert!(close(area(&u), 20000.0, 0.5));
        assert_eq!(u.subpaths.len(), 1, "one outline, the shared edge gone");
        assert!(combine(&[a.clone(), b], CombineOp::Intersect).is_none(), "touching is not overlapping");
        // The same rectangle twice.
        assert!(close(area(&combine(&[a.clone(), a.clone()], CombineOp::Union).unwrap()), 10000.0, 0.5));
        assert!(combine(&[a.clone(), a], CombineOp::Subtract).is_none());
    }

    #[test]
    fn a_figure_eight_crossing_itself() {
        let eight = PathGeom::polyline(&[Point::new(0.0, 0.0), Point::new(100.0, 100.0), Point::new(100.0, 0.0), Point::new(0.0, 100.0)], true);
        let far = rect(500.0, 500.0, 10.0, 10.0);
        let u = combine(&[eight.clone(), far], CombineOp::Union).unwrap();
        assert!(close(area(&u), 5100.0, 0.5), "{}", area(&u));
        // Cut through its crossing.
        let left = rect(0.0, 0.0, 50.0, 100.0);
        let i = combine(&[eight, left], CombineOp::Intersect).unwrap();
        assert!(close(area(&i), 2500.0, 0.5), "{}", area(&i));
    }

    #[test]
    fn a_shape_with_a_hole_keeps_it() {
        let mut holed = rect(0.0, 0.0, 100.0, 100.0);
        holed.subpaths.extend(rect(25.0, 25.0, 50.0, 50.0).subpaths);
        holed.rule = PathRule::EvenOdd;
        assert!(close(area(&holed), 7500.0, 0.5));
        let half = rect(50.0, 0.0, 100.0, 100.0);
        let i = combine(&[holed.clone(), half.clone()], CombineOp::Intersect).unwrap();
        assert!(close(area(&i), 3750.0, 0.5), "{}", area(&i));
        let u = combine(&[holed, half], CombineOp::Union).unwrap();
        // The hole's left half stays a hole.
        assert!(close(area(&u), 15000.0 - 1250.0, 0.5), "{}", area(&u));
        assert!(!u.contains(Point::new(40.0, 50.0)));
        assert!(u.contains(Point::new(10.0, 50.0)));
    }

    #[test]
    fn a_star_minus_a_circle() {
        let star = path::star(&Rect::new(-100.0, -100.0, 200.0, 200.0), 5, 0.4);
        let c = circle(0.0, 0.0, 30.0);
        let d = combine(&[star.clone(), c], CombineOp::Subtract).unwrap();
        let expect = area(&star) - std::f32::consts::PI * 900.0;
        assert!(close(area(&d), expect, 5.0), "{} vs {}", area(&d), expect);
        assert!(!d.contains(Point::new(0.0, 0.0)), "the middle is cut out");
    }

    #[test]
    fn three_operands_fold_bottom_to_top() {
        let a = rect(0.0, 0.0, 300.0, 100.0);
        let b = rect(20.0, 20.0, 50.0, 50.0);
        let c = rect(200.0, 20.0, 50.0, 50.0);
        let s = combine(&[a.clone(), b.clone(), c.clone()], CombineOp::Subtract).unwrap();
        assert!(close(area(&s), 30000.0 - 5000.0, 0.5));
        let x = combine(&[a, b, c], CombineOp::Exclude).unwrap();
        assert!(close(area(&x), 30000.0 - 5000.0, 0.5));
    }

    #[test]
    fn two_hundred_anchors_combine_quickly() {
        let wobbly = |cx: f32, k: f32| {
            let pts: Vec<Point> = (0..200)
                .map(|i| {
                    let t = i as f32 / 200.0 * std::f32::consts::TAU;
                    let r = 100.0 * (1.0 + 0.08 * (k * t).sin());
                    Point::new(cx + r * t.cos(), r * t.sin())
                })
                .collect();
            // Smooth through the points: 200 curved anchors.
            let mut p = PathGeom::polyline(&pts, true);
            let sp = &mut p.subpaths[0];
            for i in 0..200 {
                let (prev, next) = (pts[(i + 199) % 200], pts[(i + 1) % 200]);
                let h = Point::new((next.x - prev.x) / 6.0, (next.y - prev.y) / 6.0);
                sp.anchors[i].h_out = h;
                sp.anchors[i].h_in = Point::new(-h.x, -h.y);
            }
            p
        };
        let (a, b) = (wobbly(0.0, 13.0), wobbly(40.0, 17.0));
        let t = std::time::Instant::now();
        for op in CombineOp::ALL {
            assert!(combine(&[a.clone(), b.clone()], op).is_some());
        }
        // Measured about 10 ms for all four in release (the spike: 2.5 ms
        // each for curves); generous for a loaded test machine.
        assert!(t.elapsed().as_millis() < 2000, "{:?}", t.elapsed());
    }
}
