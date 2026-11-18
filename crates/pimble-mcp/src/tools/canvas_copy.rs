//! Reading a canvas's elements whole, and copying them between canvases
//! (docs/MCP_CONTRACT.md "Canvases", docs/DRAWING_CONTRACT.md "Large
//! canvases"): `get_elements` gives each element in the shape
//! `add_elements` takes, so an agent can write it elsewhere verbatim, and a
//! paint layer's strokes and fills in the shape `paint` takes;
//! `copy_elements` and `copy_layer` copy them (paint operations of every
//! kind included) inside Pimble, in one edit of the target canvas.

use std::collections::{HashMap, HashSet};

use pimble_board::{
    BoardError, BoardSnapshot, BoardTxn, Element, End, Geom, Gradient, LayerKind, NewElement, PaintBody, PaintOp, PixelTransform, Placement, Point,
    Rect, Selection, Style, Transform,
};
use pimble_board::path::to_d;
use serde_json::{json, Map, Value};

use super::{bad, Scene};

/// A number as JSON, rounded to three decimals (canvas units).
fn num(v: f32) -> Value {
    let r = (v as f64 * 1000.0).round() / 1000.0;
    if r == r.trunc() && r.abs() < 1e15 {
        json!(r as i64)
    } else {
        json!(r)
    }
}

fn pt(p: Point) -> Value {
    json!([num(p.x), num(p.y)])
}

fn gradient_json(g: &Gradient) -> Value {
    json!({
        "type": g.kind.as_str(),
        "from": pt(g.from),
        "to": pt(g.to),
        "stops": g.stops.iter().map(|s| json!({"offset": num(s.offset), "color": s.color.to_hex()})).collect::<Vec<_>>(),
    })
}

/// An element's own style as add_elements takes it (only what is set).
pub(super) fn style_json(style: &Style) -> Value {
    let mut m = Map::new();
    if let Some(c) = style.stroke {
        m.insert("stroke".into(), json!(c.to_hex()));
    }
    if let Some(c) = style.fill {
        m.insert("fill".into(), json!(if c.a == 0 { "none".to_string() } else { c.to_hex() }));
    }
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            m.insert(k.into(), v);
        }
    };
    put("width", style.width.map(num));
    put("opacity", style.opacity.map(num));
    put("font_size", style.font_size.map(num));
    put("align", style.align.map(|a| json!(a.as_str())));
    put("shadow", style.shadow.map(|s| json!(s)));
    put("arrow_heads", style.arrow_heads.map(|h| json!(h.as_str())));
    put(
        "dash",
        style.dash_pattern.as_ref().map(|p| json!(pimble_board::dash_pattern_text(p))).or(style.dash.map(|d| json!(d.as_str()))),
    );
    put("cap", style.cap.map(|c| json!(c.as_str())));
    put("join", style.join.map(|j| json!(j.as_str())));
    put("miter", style.miter.map(num));
    put("radius", style.radius.map(num));
    put("sides", style.sides.map(|v| json!(v)));
    put("points", style.points.map(|v| json!(v)));
    put("inner", style.inner.map(num));
    put("fill_gradient", style.fill_gradient.as_ref().map(gradient_json));
    put("stroke_gradient", style.stroke_gradient.as_ref().map(gradient_json));
    Value::Object(m)
}

fn rect_into(m: &mut Map<String, Value>, r: &Rect) {
    m.insert("x".into(), num(r.x));
    m.insert("y".into(), num(r.y));
    m.insert("w".into(), num(r.w));
    m.insert("h".into(), num(r.h));
}

fn end_into(m: &mut Map<String, Value>, end: &End, which: &str) {
    match end {
        End::Element { id, side, .. } => {
            m.insert(which.into(), json!(id));
            if let Some(side) = side {
                m.insert(format!("{which}_side"), json!(side.as_str()));
            }
        }
        End::Point(p) => {
            m.insert(format!("{which}_point"), pt(*p));
        }
    }
}

/// One element, complete, in the shape add_elements takes (plus its `id`;
/// its `ref` is its id, so a parent or connector end named in the same
/// list resolves when they are added together). `layer` is its layer's name.
pub(super) fn element_json(e: &Element, layer: &str) -> Value {
    let raw = &e.raw;
    let mut m = Map::new();
    m.insert("id".into(), json!(raw.id));
    m.insert("ref".into(), json!(raw.id));
    m.insert("kind".into(), json!(raw.kind.as_str()));
    match &raw.geom {
        Geom::None => {}
        Geom::Rect(r) => rect_into(&mut m, r),
        Geom::Picture(p) => {
            rect_into(&mut m, &p.rect);
            m.insert("src".into(), json!(p.src));
            if let Some(c) = p.crop {
                m.insert("crop".into(), json!({"x": num(c.x), "y": num(c.y), "w": num(c.w), "h": num(c.h)}));
            }
        }
        Geom::Segment { a, b } => {
            m.insert("points".into(), json!([pt(*a), pt(*b)]));
        }
        Geom::Anchor(p) => {
            m.insert("x".into(), num(p.x));
            m.insert("y".into(), num(p.y));
        }
        Geom::Callout { rect, tip } => {
            rect_into(&mut m, rect);
            m.insert("tip".into(), pt(*tip));
        }
        Geom::Connector { from, to } => {
            end_into(&mut m, from, "from");
            end_into(&mut m, to, "to");
        }
        Geom::Card { rect, link } => {
            rect_into(&mut m, rect);
            m.insert("node".into(), json!(link));
        }
        Geom::Stroke => {
            let points: Vec<Value> = raw.points.iter().map(|p| json!([num(p[0]), num(p[1]), num(p[2])])).collect();
            m.insert("points".into(), Value::Array(points));
        }
        Geom::Path(p) => {
            m.insert("d".into(), json!(to_d(p)));
            if p.rule == pimble_board::PathRule::EvenOdd {
                m.insert("rule".into(), json!("evenodd"));
            }
        }
    }
    if let Some(text) = &raw.text {
        m.insert("text".into(), json!(text));
    }
    let style = style_json(&raw.style);
    if style.as_object().is_some_and(|o| !o.is_empty()) {
        m.insert("style".into(), style);
    }
    let t = raw.transform;
    if !t.is_identity() {
        m.insert("transform".into(), json!({"tx": num(t.tx), "ty": num(t.ty), "rot": num(t.rot), "sx": num(t.sx), "sy": num(t.sy)}));
    }
    m.insert("layer".into(), json!(layer));
    if let Some(parent) = &e.parent {
        m.insert("parent".into(), json!(parent));
    }
    if raw.locked {
        m.insert("locked".into(), json!(true));
    }
    if let Some(note) = raw.note.as_deref().filter(|n| !n.trim().is_empty()) {
        m.insert("note".into(), json!(note));
    }
    if !raw.fields.is_empty() {
        let fields: Map<String, Value> = raw.fields.iter().map(|(n, v)| (n.clone(), json!(v))).collect();
        m.insert("fields".into(), Value::Object(fields));
    }
    Value::Object(m)
}

/// A paint operation in the shape `paint` takes: `("stroke", spec)` or
/// `("fill", spec)`; `None` for the kinds paint does not take (pictures,
/// smudges, blurs, moves), which copy_layer copies.
pub(super) fn paint_json(op: &PaintOp) -> Option<(&'static str, Value)> {
    match &op.raw.body {
        PaintBody::Stroke(s) => {
            let points: Vec<Value> = s.points.iter().map(|p| json!([num(p[0]), num(p[1]), num(p[2])])).collect();
            let mut m = Map::new();
            m.insert("id".into(), json!(op.raw.id));
            m.insert("preset".into(), json!(s.preset));
            m.insert("color".into(), json!(s.color.to_hex()));
            m.insert("size".into(), num(s.settings.size));
            m.insert("opacity".into(), num(s.settings.opacity));
            if s.blend == pimble_board::StrokeBlend::Erase {
                m.insert("erase".into(), json!(true));
            }
            m.insert("points".into(), Value::Array(points));
            Some(("stroke", Value::Object(m)))
        }
        PaintBody::Fill(f) => Some((
            "fill",
            json!({
                "id": op.raw.id,
                "at": pt(f.seed),
                "color": f.color.to_hex(),
                "tolerance": num(f.tolerance),
                "contiguous": f.contiguous,
                "area": {"x": num(f.clip.x), "y": num(f.clip.y), "w": num(f.clip.w), "h": num(f.clip.h)},
            }),
        )),
        _ => None,
    }
}

/// `ids` and every element inside them (a group's or frame's members, at
/// any depth), in z-order (bottom to top). Refuses an id the canvas does
/// not have.
pub(super) fn with_members(snapshot: &BoardSnapshot, ids: &[String]) -> Result<Vec<usize>, BoardError> {
    let mut wanted: HashSet<String> = HashSet::new();
    for id in ids {
        let id = id.trim();
        if snapshot.get(id).is_none() {
            return Err(bad(format!("there is no element \"{id}\" on this canvas (get_canvas lists the ids)")));
        }
        wanted.insert(id.to_string());
    }
    // Members, until nothing new joins.
    loop {
        let before = wanted.len();
        for e in &snapshot.elements {
            if e.parent.as_ref().is_some_and(|p| wanted.contains(p)) {
                wanted.insert(e.raw.id.clone());
            }
        }
        if wanted.len() == before {
            break;
        }
    }
    Ok(snapshot.elements.iter().enumerate().filter(|(_, e)| wanted.contains(&e.raw.id)).map(|(i, _)| i).collect())
}

/// The elements of the layers `layers` (ids), in z-order.
pub(super) fn of_layers(snapshot: &BoardSnapshot, layers: &HashSet<String>) -> Vec<usize> {
    snapshot.elements.iter().enumerate().filter(|(_, e)| layers.contains(&e.layer)).map(|(i, _)| i).collect()
}

/// A paint operation moved by `d` (`None`: a kind of a later version).
pub(super) fn moved_body(body: &PaintBody, d: Point) -> Option<PaintBody> {
    let shift = |p: Point| Point::new(p.x + d.x, p.y + d.y);
    let shift_rect = |r: &Rect| Rect::new(r.x + d.x, r.y + d.y, r.w, r.h);
    Some(match body {
        PaintBody::Other(_) => return None,
        _ if d.x == 0.0 && d.y == 0.0 => body.clone(),
        PaintBody::Stroke(s) => {
            let mut s = (**s).clone();
            for p in &mut s.points {
                p[0] += d.x;
                p[1] += d.y;
            }
            PaintBody::Stroke(std::sync::Arc::new(s))
        }
        PaintBody::Retouch(r) => {
            let mut r = (**r).clone();
            for p in &mut r.points {
                p[0] += d.x;
                p[1] += d.y;
            }
            PaintBody::Retouch(std::sync::Arc::new(r))
        }
        PaintBody::Pixels(p) => {
            let mut p = p.clone();
            p.rect = shift_rect(&p.rect);
            PaintBody::Pixels(p)
        }
        PaintBody::Fill(f) => {
            let mut f = f.clone();
            f.seed = shift(f.seed);
            f.clip = shift_rect(&f.clip);
            PaintBody::Fill(f)
        }
        PaintBody::Move(m) => {
            let mut m = m.clone();
            m.selection = match &m.selection {
                Selection::Rect(r) => Selection::Rect(shift_rect(r)),
                Selection::Lasso(points) => Selection::Lasso(points.iter().map(|p| shift(*p)).collect()),
            };
            // The same move where it now is: back to where it was, the
            // move, and out again.
            m.transform = m.transform.map(|t| PixelTransform::translate(-d.x, -d.y).then(&t).then(&PixelTransform::translate(d.x, d.y)));
            PaintBody::Move(m)
        }
    })
}

/// The pictures (`pimble-blob:` URLs) what is copied shows.
pub(super) fn picture_srcs(snapshot: &BoardSnapshot, elements: &[usize], paint_layers: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for &i in elements {
        if let Geom::Picture(p) = &snapshot.elements[i].raw.geom {
            if !out.contains(&p.src) {
                out.push(p.src.clone());
            }
        }
    }
    for l in paint_layers {
        for op in snapshot.paint_ops(l) {
            if let PaintBody::Pixels(p) = &op.raw.body {
                if !out.contains(&p.src) {
                    out.push(p.src.clone());
                }
            }
        }
    }
    out
}

/// What one copy writes: elements (by index into the source snapshot) into
/// a layer each, and paint layers' operations into a layer each.
pub(super) struct CopyPlan {
    /// (source element index, target layer id).
    pub elements: Vec<(usize, String)>,
    /// (source paint layer id, target paint layer id).
    pub paint: Vec<(String, String)>,
    pub offset: Point,
    /// A picture's URL in the source's store, as stored in the target's.
    pub srcs: HashMap<String, String>,
}

/// What a copy made.
#[derive(Default)]
pub(super) struct Copied {
    /// (old id, new id) of each element, in z-order.
    pub elements: Vec<(String, String)>,
    pub ops: usize,
    /// Operations of a later version left out.
    pub skipped: usize,
}

/// Write `plan` from `source` in one transaction: new ids, parents and
/// connector ends among the copied elements mapped to the copies (an end on
/// an element not copied becomes a point where it was), the copies moved by
/// the offset (a member moves with its copied parent), notes, fields and
/// locks kept (locks last), paint operations laid on top in their order.
pub(super) fn write_copy(t: &mut BoardTxn<'_, '_>, scene: &mut Scene, source: &BoardSnapshot, plan: &CopyPlan) -> Result<Copied, BoardError> {
    let d = plan.offset;
    let copied: HashSet<&str> = plan.elements.iter().map(|(i, _)| source.elements[*i].raw.id.as_str()).collect();
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut out = Copied::default();
    // Connectors last, so the elements they join have their new ids.
    let order: Vec<&(usize, String)> = plan
        .elements
        .iter()
        .filter(|(i, _)| !matches!(source.elements[*i].raw.geom, Geom::Connector { .. }))
        .chain(plan.elements.iter().filter(|(i, _)| matches!(source.elements[*i].raw.geom, Geom::Connector { .. })))
        .collect();
    for (i, layer) in order {
        let e = &source.elements[*i];
        let raw = &e.raw;
        let parent_copied = e.parent.as_deref().is_some_and(|p| copied.contains(p));
        let shift = |p: Point| Point::new(p.x + d.x, p.y + d.y);
        let mut transform = raw.transform;
        let geom = match &raw.geom {
            Geom::Connector { from, to } => {
                let end = |end: &End| match end {
                    End::Element { id, side, last } => match ids.get(id) {
                        Some(new) => End::Element { id: new.clone(), side: *side, last: shift(*last) },
                        None => End::Point(shift(*last)),
                    },
                    End::Point(p) => End::Point(shift(*p)),
                };
                Geom::Connector { from: end(from), to: end(to) }
            }
            Geom::Picture(p) => {
                let mut p = p.clone();
                if let Some(src) = plan.srcs.get(&p.src) {
                    p.src = src.clone();
                }
                if !parent_copied {
                    transform.tx += d.x;
                    transform.ty += d.y;
                }
                Geom::Picture(p)
            }
            other => {
                if !parent_copied {
                    transform.tx += d.x;
                    transform.ty += d.y;
                }
                other.clone()
            }
        };
        let mut element = NewElement::new(raw.kind.clone(), geom).with_points(raw.points.clone()).with_style(raw.style.clone()).in_layer(layer.clone());
        if let Some(text) = &raw.text {
            element = element.with_text(text.clone());
        }
        if transform != Transform::IDENTITY {
            element = element.with_transform(transform);
        }
        let id = t.add_element(element)?;
        scene.touched.push(id.clone());
        ids.insert(raw.id.clone(), id);
    }
    for (i, _) in &plan.elements {
        let e = &source.elements[*i];
        let id = &ids[&e.raw.id];
        if let Some(parent) = e.parent.as_ref().and_then(|p| ids.get(p)) {
            t.set_parent(id, Some(parent))?;
        }
        if let Some(note) = e.raw.note.as_deref().filter(|n| !n.trim().is_empty()) {
            t.set_note(id, note)?;
        }
        for (name, value) in &e.raw.fields {
            t.set_field(id, name, Some(value))?;
        }
    }
    for (i, _) in &plan.elements {
        let e = &source.elements[*i];
        if e.raw.locked {
            t.set_locked(&ids[&e.raw.id], true)?;
        }
        out.elements.push((e.raw.id.clone(), ids[&e.raw.id].clone()));
    }
    for (from, to) in &plan.paint {
        for op in source.paint_ops(from) {
            let Some(mut body) = moved_body(&op.raw.body, d) else {
                out.skipped += 1;
                continue;
            };
            if let PaintBody::Pixels(p) = &mut body {
                if let Some(src) = plan.srcs.get(&p.src) {
                    p.src = src.clone();
                }
            }
            t.add_paint_body(to, body)?;
            out.ops += 1;
        }
    }
    Ok(out)
}

/// Make a copy of layer `at` of `source` (a group with every layer in it)
/// in the target canvas, inside `parent` (`None`: the top level), on top:
/// kind, name, blend, opacity, visibility, note and fields; locks after
/// everything is written (`locks`). Answers the new layer's id, and adds
/// to `plan` what goes into each new layer.
pub(super) fn copy_layer_tree(
    t: &mut BoardTxn<'_, '_>,
    source: &BoardSnapshot,
    at: usize,
    name: &str,
    parent: Option<&str>,
    plan: &mut CopyPlan,
    locks: &mut Vec<String>,
) -> Result<String, BoardError> {
    let l = &source.layers[at];
    let id = t.add_layer_in(name, l.kind.clone(), parent, Placement::Top)?;
    if l.blend != l.kind.default_blend() {
        t.set_layer_blend(&id, l.blend)?;
    }
    if l.opacity < 1.0 {
        t.set_layer_opacity(&id, l.opacity)?;
    }
    if !l.visible {
        t.set_layer_visible(&id, false)?;
    }
    if let Some(note) = l.note.as_deref().filter(|n| !n.trim().is_empty()) {
        t.set_layer_note(&id, note)?;
    }
    for (field, value) in &l.fields {
        t.set_layer_field(&id, field, Some(value))?;
    }
    if l.locked {
        locks.push(id.clone());
    }
    match l.kind {
        LayerKind::Group => {
            // Bottom first, each on top of the last.
            let tree = source.layer_tree();
            for child in tree.entries[at].children.clone() {
                let child_name = source.layers[child].name.clone();
                copy_layer_tree(t, source, child, &child_name, Some(&id), plan, locks)?;
            }
        }
        LayerKind::Paint => plan.paint.push((l.id.clone(), id.clone())),
        _ => {
            for (i, e) in source.elements.iter().enumerate() {
                if e.layer == l.id {
                    plan.elements.push((i, id.clone()));
                }
            }
        }
    }
    Ok(id)
}
