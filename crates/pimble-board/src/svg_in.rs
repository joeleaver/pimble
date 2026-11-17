//! SVG in (docs/DRAWING_CONTRACT.md section 2, "SVG in"): markup pasted or
//! an `.svg` file dropped becomes paths, groups and simple text.
//!
//! The drawing is read with `usvg`, which resolves what real files lean on
//! (CSS in `<style>` and classes, `<use>`, units and percentages, `viewBox`
//! and `preserveAspectRatio`, inherited presentation attributes) and turns
//! every shape (`rect`, `circle`, `ellipse`, `line`, `polyline`, `polygon`,
//! `path`) into path data. Each becomes a `path` element in board units,
//! its transforms applied to its geometry (so it has no transform of its
//! own), with its fill and stroke: color and opacity, linear and radial
//! gradients (from `<defs>`, by reference), width, caps, joins, miter limit
//! and dashes. A `<g>` holding two or more things becomes a board group of
//! them (a group's opacity is folded into each member's). `<text>` is read
//! off the markup itself, one text element per `<text>` (its lines
//! joined), with its position, size, color, anchor and rotation, drawn in
//! the board's one font; text goes on top of the drawing.
//!
//! What is skipped is said, once each, in [`SvgImport::warnings`]: filters,
//! masks, clip paths (the shapes come in unclipped), pictures, patterns
//! (filled grey), blend modes, repeating gradients (they pad), text on a
//! path and text styled only by a stylesheet class.

use std::collections::HashMap;

use crate::model::*;
use crate::path::{parse_d, PathGeom, PathRule};
use crate::text;
use usvg::roxmltree;

/// Where an imported drawing goes on the board.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvgPlacement {
    /// The drawing's top-left corner on the board, or its center when
    /// `center` is set (a paste at the middle of the view).
    pub at: Point,
    pub center: bool,
    /// Scale it so its longer side is this long, in board units. `None`:
    /// one SVG pixel (user unit, after `viewBox`) is one board unit.
    pub fit: Option<f32>,
}

impl Default for SvgPlacement {
    fn default() -> Self {
        Self { at: Point::new(0.0, 0.0), center: false, fit: None }
    }
}

/// One thing to add: an element, and the group it belongs to (an index
/// into [`SvgImport::items`], always an earlier item).
#[derive(Debug, Clone)]
pub struct SvgItem {
    pub element: NewElement,
    pub group: Option<usize>,
}

/// An SVG drawing read for the board: what to add, bottom to top, and what
/// was left out.
#[derive(Debug, Clone)]
pub struct SvgImport {
    pub items: Vec<SvgItem>,
    /// Sentences, one per thing skipped ("Filters are not imported.").
    pub warnings: Vec<String>,
    /// Where it all lands, in board units.
    pub bounds: Option<Rect>,
}

/// Why markup could not be imported.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SvgError {
    #[error("That is not SVG that can be read ({0}).")]
    Parse(String),
    #[error("The SVG draws nothing that can be imported.")]
    Empty,
}

/// A 2D affine map: x' = a x + c y + e, y' = b x + d y + f (SVG's matrix).
#[derive(Debug, Clone, Copy, PartialEq)]
struct M {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl M {
    const ID: M = M { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    fn of_usvg(t: &usvg::Transform) -> M {
        M { a: t.sx as f64, b: t.ky as f64, c: t.kx as f64, d: t.sy as f64, e: t.tx as f64, f: t.ty as f64 }
    }

    fn of_svgtypes(t: &svgtypes::Transform) -> M {
        M { a: t.a, b: t.b, c: t.c, d: t.d, e: t.e, f: t.f }
    }

    /// `self` after `o` (o first).
    fn then_after(&self, o: &M) -> M {
        M {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }

    fn apply(&self, x: f64, y: f64) -> Point {
        Point::new((self.a * x + self.c * y + self.e) as f32, (self.b * x + self.d * y + self.f) as f32)
    }

    /// How much it scales lengths, on average.
    fn scale(&self) -> f32 {
        (self.a * self.d - self.b * self.c).abs().sqrt() as f32
    }
}

/// What was read, before it is placed.
enum Pending {
    Group { parent: Option<usize> },
    Path { geom: PathGeom, style: Style, parent: Option<usize> },
    Text { text: String, baseline: Point, size: f32, color: Color, anchor: Align, rot: f32 },
}

#[derive(Default)]
struct Warnings(Vec<String>);

impl Warnings {
    fn say(&mut self, s: &str) {
        if !self.0.iter().any(|w| w == s) {
            self.0.push(s.to_string());
        }
    }
}

/// Read SVG markup into elements placed by `place`. Refused when it does
/// not parse ([`SvgError::Parse`]) or draws nothing that comes in
/// ([`SvgError::Empty`]).
pub fn parse(svg: &str, place: &SvgPlacement) -> Result<SvgImport, SvgError> {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).map_err(|e| SvgError::Parse(e.to_string()))?;
    let mut warnings = Warnings::default();
    let mut pending: Vec<Pending> = Vec::new();
    walk(tree.root(), 1.0, None, true, &mut pending, &mut warnings);
    read_text(svg, &tree, &mut pending, &mut warnings);
    // Groups that ended up holding fewer than two things are dropped (their
    // members go to the group above).
    loop {
        let mut counts = vec![0usize; pending.len()];
        for p in &pending {
            if let Some(g) = parent_of(p) {
                counts[g] += 1;
            }
        }
        // And a group holding the whole drawing (usvg's group for the
        // viewBox, a file's one wrapping `<g>`) is the drawing itself.
        let top_level = pending.iter().filter(|p| parent_of(p).is_none()).count();
        let lonely = (0..pending.len()).find(|&i| {
            matches!(pending[i], Pending::Group { parent: None }) && top_level == 1 || matches!(pending[i], Pending::Group { .. }) && counts[i] < 2
        });
        let Some(lonely) = lonely else { break };
        let up = parent_of(&pending[lonely]);
        pending.remove(lonely);
        for p in &mut pending {
            let slot = match p {
                Pending::Group { parent } | Pending::Path { parent, .. } => parent,
                Pending::Text { .. } => continue,
            };
            *slot = match *slot {
                Some(g) if g == lonely => up,
                Some(g) if g > lonely => Some(g - 1),
                other => other,
            };
        }
    }
    let extent = pending.iter().filter_map(pending_bounds).reduce(|a, b| a.union(&b)).ok_or(SvgError::Empty)?;
    let s = match place.fit {
        Some(fit) if fit > 0.0 && extent.w.max(extent.h) > 0.0 => fit / extent.w.max(extent.h),
        _ => 1.0,
    };
    let origin = if place.center { Point::new(place.at.x - extent.w * s / 2.0, place.at.y - extent.h * s / 2.0) } else { place.at };
    let map = |p: Point| Point::new((p.x - extent.x) * s + origin.x, (p.y - extent.y) * s + origin.y);
    let mut items = Vec::with_capacity(pending.len());
    let mut bounds: Option<Rect> = None;
    for p in pending {
        let (element, group) = match p {
            Pending::Group { parent } => (NewElement::new(Kind::Group, Geom::None), parent),
            Pending::Path { geom, mut style, parent } => {
                let geom = geom.map(map);
                scale_style(&mut style, s);
                if let Some(b) = geom.bounds() {
                    bounds = Some(bounds.map_or(b, |a| a.union(&b)));
                }
                (NewElement::path(geom).with_style(style), parent)
            }
            Pending::Text { text: words, baseline, size, color, anchor, rot } => {
                let size = (size * s).max(1.0);
                let at = map(baseline);
                let l = text::layout(&words, size, None);
                let left = match anchor {
                    Align::Left => at.x,
                    Align::Center => at.x - l.width / 2.0,
                    Align::Right => at.x - l.width,
                };
                let top = at.y - text::first_baseline(size);
                let r = Rect::new(left, top, l.width, l.height);
                let mut el = NewElement::new(Kind::Text, Geom::Rect(Rect::new(left, top, 0.0, 0.0)))
                    .with_text(words)
                    .with_style(Style { stroke: Some(color), font_size: Some(size), ..Style::default() });
                if rot.abs() > 0.01 {
                    // Turned about the text's own center, then moved back so
                    // its baseline start stays where the SVG put it.
                    let c = r.center();
                    let (sin, cos) = rot.to_radians().sin_cos();
                    let (dx, dy) = (at.x - c.x, at.y - c.y);
                    let turned = Point::new(c.x + dx * cos - dy * sin, c.y + dx * sin + dy * cos);
                    el = el.with_transform(Transform { tx: at.x - turned.x, ty: at.y - turned.y, rot, ..Transform::IDENTITY });
                }
                bounds = Some(bounds.map_or(r, |a| a.union(&r)));
                (el, None)
            }
        };
        items.push(SvgItem { element, group });
    }
    Ok(SvgImport { items, warnings: warnings.0, bounds })
}

fn parent_of(p: &Pending) -> Option<usize> {
    match p {
        Pending::Group { parent } | Pending::Path { parent, .. } => *parent,
        Pending::Text { .. } => None,
    }
}

fn pending_bounds(p: &Pending) -> Option<Rect> {
    match p {
        Pending::Group { .. } => None,
        Pending::Path { geom, .. } => geom.bounds(),
        Pending::Text { text: words, baseline, size, anchor, .. } => {
            let l = text::layout(words, *size, None);
            let left = match anchor {
                Align::Left => baseline.x,
                Align::Center => baseline.x - l.width / 2.0,
                Align::Right => baseline.x - l.width,
            };
            Some(Rect::new(left, baseline.y - text::first_baseline(*size), l.width, l.height))
        }
    }
}

/// Widths, dashes and miter in board units after scaling by `s`.
fn scale_style(style: &mut Style, s: f32) {
    if let Some(w) = &mut style.width {
        *w *= s;
    }
    if let Some(p) = &mut style.dash_pattern {
        for v in p.iter_mut() {
            *v *= s;
        }
    }
}

fn walk(group: &usvg::Group, opacity: f32, parent: Option<usize>, root: bool, out: &mut Vec<Pending>, warnings: &mut Warnings) {
    if group.clip_path().is_some() {
        warnings.say("Clip paths are not imported: the shapes they clip come in whole.");
    }
    if group.mask().is_some() {
        warnings.say("Masks are not imported.");
    }
    if !group.filters().is_empty() {
        warnings.say("Filters (blurs, shadows) are not imported.");
    }
    if group.blend_mode() != usvg::BlendMode::Normal {
        warnings.say("Blend modes are not imported.");
    }
    let opacity = opacity * group.opacity().get();
    let here = if !root && group.children().len() >= 2 {
        out.push(Pending::Group { parent });
        Some(out.len() - 1)
    } else {
        parent
    };
    for node in group.children() {
        match node {
            usvg::Node::Group(g) => walk(g, opacity, here, false, out, warnings),
            usvg::Node::Path(p) => {
                if let Some(pending) = read_path(p, opacity, here, warnings) {
                    out.push(pending);
                }
            }
            usvg::Node::Image(_) => warnings.say("Pictures inside SVG are not imported."),
            // Text is read off the markup (read_text), whatever usvg made of it.
            usvg::Node::Text(_) => {}
        }
    }
}

fn read_path(p: &usvg::Path, opacity: f32, parent: Option<usize>, warnings: &mut Warnings) -> Option<Pending> {
    if !p.is_visible() || (p.fill().is_none() && p.stroke().is_none()) {
        return None;
    }
    let m = M::of_usvg(&p.abs_transform());
    let mut d = String::new();
    use std::fmt::Write as _;
    let pt = |q: usvg::tiny_skia_path::Point| m.apply(q.x as f64, q.y as f64);
    for seg in p.data().segments() {
        use usvg::tiny_skia_path::PathSegment as S;
        let _ = match seg {
            S::MoveTo(a) => {
                let a = pt(a);
                write!(d, "M{} {} ", a.x, a.y)
            }
            S::LineTo(a) => {
                let a = pt(a);
                write!(d, "L{} {} ", a.x, a.y)
            }
            S::QuadTo(a, b) => {
                let (a, b) = (pt(a), pt(b));
                write!(d, "Q{} {} {} {} ", a.x, a.y, b.x, b.y)
            }
            S::CubicTo(a, b, c) => {
                let (a, b, c) = (pt(a), pt(b), pt(c));
                write!(d, "C{} {} {} {} {} {} ", a.x, a.y, b.x, b.y, c.x, c.y)
            }
            S::Close => write!(d, "Z "),
        };
    }
    let mut geom = parse_d(&d).ok()?;
    let bounds = geom.bounds()?;
    let mut style = Style::default();
    match p.fill() {
        Some(fill) => {
            geom.rule = match fill.rule() {
                usvg::FillRule::NonZero => PathRule::NonZero,
                usvg::FillRule::EvenOdd => PathRule::EvenOdd,
            };
            match paint_of(fill.paint(), fill.opacity().get(), &m, &bounds, warnings) {
                (c, None) => style.fill = Some(c),
                (_, Some(g)) => style.fill_gradient = Some(g),
            }
        }
        None => {}
    }
    match p.stroke() {
        Some(stroke) => {
            match paint_of(stroke.paint(), stroke.opacity().get(), &m, &bounds, warnings) {
                (c, None) => style.stroke = Some(c),
                (_, Some(g)) => style.stroke_gradient = Some(g),
            }
            let k = m.scale();
            style.width = Some(stroke.width().get() * k);
            style.cap = Some(match stroke.linecap() {
                usvg::LineCap::Butt => Cap::Butt,
                usvg::LineCap::Round => Cap::Round,
                usvg::LineCap::Square => Cap::Square,
            });
            style.join = Some(match stroke.linejoin() {
                usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => Join::Miter,
                usvg::LineJoin::Round => Join::Round,
                usvg::LineJoin::Bevel => Join::Bevel,
            });
            let miter = stroke.miterlimit().get();
            if (miter - DEFAULT_MITER).abs() > 1e-3 {
                style.miter = Some(miter);
            }
            if let Some(dash) = stroke.dasharray() {
                let v: Vec<f32> = dash.iter().map(|x| x * k).collect();
                if v.iter().sum::<f32>() > 0.0 {
                    style.dash_pattern = Some(v);
                }
            }
        }
        // No stroke: a path's own default is a 4-unit red outline.
        None => style.width = Some(0.0),
    }
    if opacity < 0.999 {
        style.opacity = Some(opacity.clamp(0.0, 1.0));
    }
    Some(Pending::Path { geom, style, parent })
}

fn color_of(c: usvg::Color, alpha: f32) -> Color {
    Color::rgba(c.red, c.green, c.blue, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// A paint as a solid color, or a gradient in box units of `bounds` (the
/// path's extent, in the same space `m` maps into).
fn paint_of(paint: &usvg::Paint, opacity: f32, m: &M, bounds: &Rect, warnings: &mut Warnings) -> (Color, Option<Gradient>) {
    let b = Gradient::box_for(*bounds);
    let to_box = |p: Point| Point::new((p.x - b.x) / b.w, (p.y - b.y) / b.h);
    let stops = |base: &usvg::BaseGradient| -> Vec<GradientStop> {
        base.stops().iter().map(|s| GradientStop::new(s.offset().get(), color_of(s.color(), s.opacity().get() * opacity))).collect()
    };
    let spread = |base: &usvg::BaseGradient, warnings: &mut Warnings| {
        if base.spread_method() != usvg::SpreadMethod::Pad {
            warnings.say("Repeating and reflected gradients are imported as gradients that pad.");
        }
    };
    match paint {
        usvg::Paint::Color(c) => (color_of(*c, opacity), None),
        usvg::Paint::LinearGradient(lg) => {
            spread(lg, warnings);
            let full = m.then_after(&M::of_usvg(&lg.transform()));
            let g = Gradient::linear(
                to_box(full.apply(lg.x1() as f64, lg.y1() as f64)),
                to_box(full.apply(lg.x2() as f64, lg.y2() as f64)),
                stops(lg),
            );
            (g.fallback(), Some(g))
        }
        usvg::Paint::RadialGradient(rg) => {
            spread(rg, warnings);
            let full = m.then_after(&M::of_usvg(&rg.transform()));
            let center = full.apply(rg.cx() as f64, rg.cy() as f64);
            let r = rg.r().get() * full.scale();
            let edge = Point::new(center.x + r, center.y);
            let g = Gradient::radial(to_box(center), to_box(edge), stops(rg));
            (g.fallback(), Some(g))
        }
        usvg::Paint::Pattern(_) => {
            warnings.say("Patterns are not imported: what they fill is filled grey.");
            (Color::rgba(0x80, 0x80, 0x80, (opacity * 255.0).round() as u8), None)
        }
    }
}

// ── Text, read off the markup ───────────────────────────────────────────

/// Presentation properties of an element: its attributes, with its inline
/// `style` winning.
fn props<'a>(node: &roxmltree::Node<'a, 'a>) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = node.attributes().map(|a| (a.name().to_string(), a.value().to_string())).collect();
    if let Some(style) = node.attribute("style") {
        for decl in style.split(';') {
            if let Some((k, v)) = decl.split_once(':') {
                out.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    out
}

/// A property of `node` or the nearest ancestor that sets it.
fn inherited(node: &roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    node.ancestors().filter(|n| n.is_element()).find_map(|n| props(&n).get(name).cloned())
}

fn first_number(s: Option<&str>) -> Option<f64> {
    let s = s?;
    let first = s.split(|c: char| c.is_whitespace() || c == ',').find(|t| !t.is_empty())?;
    first.parse::<svgtypes::Length>().ok().map(|l| l.number)
}

fn font_size_of(node: &roxmltree::Node<'_, '_>) -> f32 {
    // Walk up resolving em and percent against the parent's size.
    let chain: Vec<roxmltree::Node> = node.ancestors().filter(|n| n.is_element()).collect();
    let mut size = 16.0f64;
    for n in chain.iter().rev() {
        if let Some(v) = props(n).get("font-size") {
            if let Ok(l) = v.parse::<svgtypes::Length>() {
                size = match l.unit {
                    svgtypes::LengthUnit::Em => l.number * size,
                    svgtypes::LengthUnit::Ex => l.number * size / 2.0,
                    svgtypes::LengthUnit::Percent => l.number / 100.0 * size,
                    svgtypes::LengthUnit::Pt => l.number * 4.0 / 3.0,
                    svgtypes::LengthUnit::Pc => l.number * 16.0,
                    svgtypes::LengthUnit::In => l.number * 96.0,
                    svgtypes::LengthUnit::Cm => l.number * 96.0 / 2.54,
                    svgtypes::LengthUnit::Mm => l.number * 96.0 / 25.4,
                    _ => l.number,
                };
            }
        }
    }
    size as f32
}

/// The root `<svg>`'s map from its user units to the drawing's pixels
/// (`viewBox` with `preserveAspectRatio`), as usvg applies it to shapes.
fn viewbox_map(root: &roxmltree::Node<'_, '_>, size: usvg::Size) -> M {
    let Some(vb) = root.attribute("viewBox").and_then(|v| v.parse::<svgtypes::ViewBox>().ok()) else { return M::ID };
    if vb.w <= 0.0 || vb.h <= 0.0 {
        return M::ID;
    }
    let (w, h) = (size.width() as f64, size.height() as f64);
    let aspect = root.attribute("preserveAspectRatio").and_then(|v| v.parse::<svgtypes::AspectRatio>().ok()).unwrap_or_default();
    let (mut sx, mut sy) = (w / vb.w, h / vb.h);
    use svgtypes::Align as A;
    if aspect.align != A::None {
        let k = if aspect.slice { sx.max(sy) } else { sx.min(sy) };
        sx = k;
        sy = k;
    }
    let (fx, fy) = match aspect.align {
        A::None | A::XMinYMin => (0.0, 0.0),
        A::XMidYMin => (0.5, 0.0),
        A::XMaxYMin => (1.0, 0.0),
        A::XMinYMid => (0.0, 0.5),
        A::XMidYMid => (0.5, 0.5),
        A::XMaxYMid => (1.0, 0.5),
        A::XMinYMax => (0.0, 1.0),
        A::XMidYMax => (0.5, 1.0),
        A::XMaxYMax => (1.0, 1.0),
    };
    let (ox, oy) = ((w - vb.w * sx) * fx, (h - vb.h * sy) * fy);
    M { a: sx, b: 0.0, c: 0.0, d: sy, e: ox - vb.x * sx, f: oy - vb.y * sy }
}

/// Every `<text>` drawn (not inside `<defs>`, a clip path, a mask, a
/// symbol, a pattern or a marker), as text elements on top.
fn read_text(svg: &str, tree: &usvg::Tree, out: &mut Vec<Pending>, warnings: &mut Warnings) {
    let Ok(doc) = roxmltree::Document::parse_with_options(svg, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }) else { return };
    let root = doc.root_element();
    if root.descendants().any(|n| n.is_element() && n.tag_name().name() == "image") {
        warnings.say("Pictures inside SVG are not imported.");
    }
    let vb = viewbox_map(&root, tree.size());
    for node in root.descendants().filter(|n| n.is_element() && n.tag_name().name() == "text") {
        if node.ancestors().skip(1).any(|a| matches!(a.tag_name().name(), "defs" | "clipPath" | "mask" | "symbol" | "pattern" | "marker")) {
            continue;
        }
        if node.descendants().any(|d| d.tag_name().name() == "textPath") {
            warnings.say("Text on a path is imported as plain text.");
        }
        // Its words: a tspan that sets its own y (or dy) starts a new line.
        let mut lines: Vec<String> = vec![String::new()];
        for d in node.descendants() {
            if d.is_element() && d.tag_name().name() == "tspan" && d != node && (d.attribute("y").is_some() || d.attribute("dy").is_some()) && !lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.push(String::new());
            }
            if let Some(t) = d.text().filter(|_| d.is_text()) {
                let line = lines.last_mut().expect("one line");
                line.push_str(t);
            }
        }
        let words: Vec<String> = lines.iter().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect();
        if words.is_empty() {
            continue;
        }
        if node.attribute("class").is_some() && inherited(&node, "fill").is_none() && inherited(&node, "font-size").is_none() {
            warnings.say("Text styled by a stylesheet class keeps its words, not its look.");
        }
        let first_tspan = node.descendants().find(|d| d.is_element() && d.tag_name().name() == "tspan");
        let x = first_number(node.attribute("x")).or_else(|| first_tspan.and_then(|t| first_number(t.attribute("x")))).unwrap_or(0.0);
        let y = first_number(node.attribute("y")).or_else(|| first_tspan.and_then(|t| first_number(t.attribute("y")))).unwrap_or(0.0);
        // The transforms from the root down to the text, then the viewBox.
        let mut m = M::ID;
        for a in node.ancestors().filter(|n| n.is_element()) {
            if let Some(t) = a.attribute("transform").and_then(|t| t.parse::<svgtypes::Transform>().ok()) {
                m = M::of_svgtypes(&t).then_after(&m);
            }
        }
        let m = vb.then_after(&m);
        let color = match inherited(&node, "fill").as_deref().map(str::trim) {
            Some("none") | None => Color::BLACK,
            Some(v) => v.parse::<svgtypes::Color>().map(|c| Color::rgba(c.red, c.green, c.blue, c.alpha)).unwrap_or(Color::BLACK),
        };
        let anchor = match inherited(&node, "text-anchor").as_deref().map(str::trim) {
            Some("middle") => Align::Center,
            Some("end") => Align::Right,
            _ => Align::Left,
        };
        let size = font_size_of(&node) * m.scale();
        let rot = (m.b.atan2(m.a).to_degrees()) as f32;
        out.push(Pending::Text { text: words.join("\n"), baseline: m.apply(x, y), size, color, anchor, rot });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(svg: &str) -> SvgImport {
        parse(svg, &SvgPlacement::default()).expect("parses")
    }

    fn paths(i: &SvgImport) -> Vec<(&PathGeom, &Style)> {
        i.items
            .iter()
            .filter_map(|it| match &it.element.geom {
                Geom::Path(p) => Some((p, &it.element.style)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn shapes_become_paths_in_place() {
        let i = read(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
                <rect x="10" y="10" width="80" height="40" fill="#ff0000"/>
                <circle cx="150" cy="50" r="25" fill="none" stroke="#0000ff" stroke-width="4"/>
                <line x1="0" y1="90" x2="200" y2="90" stroke="black" stroke-linecap="square" stroke-dasharray="5 5"/>
                <polyline points="0,0 10,10 20,0" fill="none" stroke="green"/>
                <polygon points="100,0 110,10 90,10" fill="#00ff0080"/>
                <ellipse cx="50" cy="80" rx="10" ry="5"/>
                <path d="M 0 0 h 10 v 10 z" fill-rule="evenodd"/>
            </svg>"##,
        );
        let ps = paths(&i);
        assert_eq!(ps.len(), 7);
        // Placed at the origin by the drawing's extent (the line from x 0, the
        // polyline from y 0): the rect keeps its place.
        let (rect, style) = ps[0];
        let b = rect.bounds().unwrap();
        assert!((b.x - 10.0).abs() < 0.01 && (b.w - 80.0).abs() < 0.01, "{b:?}");
        assert_eq!(style.fill, Some(Color::rgb(255, 0, 0)));
        assert_eq!(style.width, Some(0.0), "no stroke");
        let (_, circle) = ps[1];
        assert_eq!(circle.stroke, Some(Color::rgb(0, 0, 255)));
        assert_eq!(circle.width, Some(4.0));
        assert_eq!(circle.fill, None);
        let (_, line) = ps[2];
        assert_eq!(line.cap, Some(Cap::Square));
        assert_eq!(line.dash_pattern, Some(vec![5.0, 5.0]));
        assert_eq!(ps[4].1.fill, Some(Color::rgba(0, 255, 0, 128)));
        assert_eq!(ps[6].0.rule, PathRule::EvenOdd);
        assert!(i.warnings.is_empty(), "{:?}", i.warnings);
    }

    #[test]
    fn groups_transforms_and_viewbox() {
        let i = read(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="400" viewBox="0 0 100 100">
                <g transform="translate(10 10)" opacity="0.5">
                    <rect width="10" height="10"/>
                    <rect x="20" width="10" height="10" transform="rotate(45 25 5)"/>
                </g>
                <rect x="80" y="80" width="10" height="10"/>
            </svg>"##,
        );
        // A group of two, its members, and the lone rect.
        assert_eq!(i.items.len(), 4);
        assert_eq!(i.items[0].element.kind, Kind::Group);
        assert_eq!(i.items[1].group, Some(0));
        assert_eq!(i.items[2].group, Some(0));
        assert_eq!(i.items[3].group, None);
        // viewBox scales by 4; the drawing starts at (40, 40) and lands at 0.
        let b = match &i.items[1].element.geom {
            Geom::Path(p) => p.bounds().unwrap(),
            _ => panic!(),
        };
        assert!((b.w - 40.0).abs() < 0.01 && b.x.abs() < 0.01, "{b:?}");
        assert_eq!(i.items[1].element.style.opacity, Some(0.5));
        // The turned square is a diamond: wider than its side.
        let d = match &i.items[2].element.geom {
            Geom::Path(p) => p.bounds().unwrap(),
            _ => panic!(),
        };
        assert!(d.w > 50.0, "{d:?}");
    }

    #[test]
    fn gradients_by_reference_and_css_classes() {
        let i = read(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
                <style>.blue { fill: #0000ff }</style>
                <defs>
                  <linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue" stop-opacity="0.5"/></linearGradient>
                  <radialGradient id="r" gradientUnits="userSpaceOnUse" cx="50" cy="50" r="25"><stop offset="0" stop-color="white"/><stop offset="1" stop-color="black"/></radialGradient>
                </defs>
                <rect width="100" height="50" fill="url(#g)"/>
                <rect y="50" width="100" height="50" fill="url(#r)" stroke="url(#g)"/>
                <rect class="blue" x="40" y="40" width="20" height="20"/>
            </svg>"##,
        );
        let ps = paths(&i);
        let g = ps[0].1.fill_gradient.as_ref().expect("a linear gradient");
        assert_eq!(g.kind, GradientKind::Linear);
        assert!((g.from.x).abs() < 1e-3 && (g.to.x - 1.0).abs() < 1e-3, "{g:?}");
        assert_eq!(g.stops[1].color, Color::rgba(0, 0, 255, 128));
        let r = ps[1].1.fill_gradient.as_ref().expect("a radial gradient");
        assert_eq!(r.kind, GradientKind::Radial);
        // Centered at (50, 50) of a box from y 50: box units (0.5, 0).
        assert!((r.from.x - 0.5).abs() < 1e-3 && r.from.y.abs() < 1e-3, "{r:?}");
        assert!((r.to.x - 0.75).abs() < 1e-3, "radius 25 of a 100 wide box");
        assert!(ps[1].1.stroke_gradient.is_some());
        assert_eq!(ps[2].1.fill, Some(Color::rgb(0, 0, 255)), "a class from <style>");
    }

    #[test]
    fn text_comes_in_as_text_and_the_rest_is_said() {
        let i = read(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="100">
                <defs><filter id="f"><feGaussianBlur stdDeviation="2"/></filter><text>hidden</text></defs>
                <rect width="10" height="10" filter="url(#f)"/>
                <text x="150" y="50" font-size="20" fill="#336699" text-anchor="middle">Hello <tspan>world</tspan></text>
                <image href="data:image/png;base64,AAAA" width="10" height="10"/>
            </svg>"##,
        );
        let texts: Vec<&NewElement> = i.items.iter().map(|it| &it.element).filter(|e| e.kind == Kind::Text).collect();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].text.as_deref(), Some("Hello world"));
        assert_eq!(texts[0].style.font_size, Some(20.0));
        assert_eq!(texts[0].style.stroke, Some(Color::rgb(0x33, 0x66, 0x99)));
        assert!(i.warnings.iter().any(|w| w.contains("Filters")), "{:?}", i.warnings);
        assert!(i.warnings.iter().any(|w| w.contains("Pictures")), "{:?}", i.warnings);
    }

    #[test]
    fn placed_centered_and_fitted() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="20"><rect width="50" height="20" stroke="red" stroke-width="2"/></svg>"##;
        let i = parse(svg, &SvgPlacement { at: Point::new(1000.0, 1000.0), center: true, fit: Some(100.0) }).unwrap();
        let (p, style) = paths(&i)[0];
        let b = p.bounds().unwrap();
        assert!((b.w - 100.0).abs() < 0.01 && (b.h - 40.0).abs() < 0.01, "{b:?}");
        assert!((b.center().x - 1000.0).abs() < 0.01 && (b.center().y - 1000.0).abs() < 0.01);
        assert_eq!(style.width, Some(4.0), "the stroke scales with it");
    }

    #[test]
    fn refusals() {
        assert!(matches!(parse("not svg", &SvgPlacement::default()), Err(SvgError::Parse(_))));
        assert_eq!(parse(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#, &SvgPlacement::default()).unwrap_err(), SvgError::Empty);
    }
}
